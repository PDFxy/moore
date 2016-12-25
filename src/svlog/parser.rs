// Copyright (c) 2016 Fabian Schuiki

//! A parser for the SystemVerilog language. Based on IEEE 1800-2009.

use svlog::lexer::{Lexer, TokenAndSpan};
use svlog::token::*;
use std::collections::VecDeque;
use errors::*;
use svlog::ast::*;
use name::*;
use source::*;

// The problem with data_declaration and data_type_or_implicit:
//
//     [7:0] foo;            # implicit "[7:0]", var "foo"
//     foo bar;              # explicit "foo", var "bar"
//     foo [7:0];            # implicit, var "foo[7:0]"
//     foo [7:0] bar [7:0];  # explicit "foo[7:0]", var "bar[7:0]"


/// Return type of the lower parse primitives, allowing for further adjustment
/// of the diagnostic message that would be generated.
type ParseResult<T> = Result<T, DiagBuilder2>;

/// Return type of functions that emit diagnostic messages and only need to
/// communicate success to the parent.
type ReportedResult<T> = Result<T, ()>;


struct Parser<'a> {
	input: Lexer<'a>,
	queue: VecDeque<TokenAndSpan>,
	diagnostics: Vec<DiagBuilder2>,
	last_span: Span,
}

impl<'a> Parser<'a> {
	fn new(input: Lexer) -> Parser {
		Parser {
			input: input,
			queue: VecDeque::new(),
			diagnostics: Vec::new(),
			last_span: INVALID_SPAN,
		}
	}

	fn ensure_queue_filled(&mut self, min_tokens: usize) {
		if let Some(&(Eof,_)) = self.queue.back() {
			return;
		}
		while self.queue.len() <= min_tokens {
			match self.input.next_token() {
				Ok((Eof, sp)) => self.queue.push_back((Eof, sp)),
				Ok(tkn) => self.queue.push_back(tkn),
				Err(x) => self.add_diag(x),
			}
		}
	}

	fn peek(&mut self, offset: usize) -> TokenAndSpan {
		self.ensure_queue_filled(offset);
		if offset < self.queue.len() {
			self.queue[offset]
		} else {
			*self.queue.back().expect("At least an Eof token should be in the queue")
		}
	}

	fn bump(&mut self) {
		if self.queue.is_empty() {
			self.ensure_queue_filled(1);
		}
		if let Some((_,sp)) = self.queue.pop_front() {
			self.last_span = sp;
		}
	}

	fn last_span(&self) -> Span {
		self.last_span
	}

	fn add_diag(&mut self, diag: DiagBuilder2) {
		// println!("*** {:?}", diag);
		println!("{}: {}", diag.get_severity(), diag.get_message());

		// Dump the part of the source file that is affected.
		if let Some(sp) = diag.get_span() {
			let c = sp.source.get_content();
			let mut iter = c.extract_iter(0, sp.begin);

			// Look for the start of the line.
			let mut col = 1;
			let mut line = 1;
			let mut line_offset = 0;
			while let Some(c) = iter.next_back() {
				match c.1 {
					'\n' => { line += 1; break; },
					'\r' => continue,
					_ => {
						col += 1;
						line_offset = c.0;
					}
				}
			}

			// Count the number of lines.
			while let Some(c) = iter.next_back() {
				if c.1 == '\n' {
					line += 1;
				}
			}

			let text: String = c.iter_from(line_offset).map(|x| x.1).take_while(|c| *c != '\n' && *c != '\r').collect();
			println!("{}:{}:{}-{}:", sp.source.get_path(), line, col, col + sp.extract().len());
			for c in text.chars() {
				match c {
					'\t' => print!("    "),
					c => print!("{}", c),
				}
			}
			print!("\n");

			for (mut i,c) in text.char_indices() {
				i += line_offset;
				let d = if (i >= sp.begin && i < sp.end) || (i == sp.begin && sp.begin == sp.end) {
					'^'
				} else {
					' '
				};
				match c {
					'\t' => print!("{}{}{}{}", d, d, d, d),
					_ => print!("{}", d),
				}
			}
			print!("\n\n");
		}

		self.diagnostics.push(diag);
		// TODO: Keep track of the worst diagnostic encountered, such that fatal
		// errors can properly abort parsing.
	}

	fn get_diagnostics(&self) -> &[DiagBuilder2] {
		&self.diagnostics
	}

	fn is_fatal(&self) -> bool {
		false
	}

	fn try_eat_ident(&mut self) -> Option<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Some((name, span)) },
			(EscIdent(name), span) => { self.bump(); Some((name, span)) },
			_ => None,
		}
	}

	fn eat_ident_or(&mut self, msg: &str) -> ParseResult<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Ok((name, span)) },
			(EscIdent(name), span) => { self.bump(); Ok((name, span)) },
			(tkn, span) => Err(DiagBuilder2::error(format!("Expected {} before {:?}", msg, tkn)).span(span)),
		}
	}

	fn eat_ident(&mut self, msg: &str) -> ReportedResult<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Ok((name, span)) }
			(EscIdent(name), span) => { self.bump(); Ok((name, span)) }
			(tkn, span) => {
				self.add_diag(DiagBuilder2::error(format!("Expected {} before {:?}", msg, tkn)).span(span));
				Err(())
			}
		}
	}

	fn require(&mut self, expect: Token) -> Result<(), DiagBuilder2> {
		match self.peek(0) {
			(actual, _) if actual == expect => { self.bump(); Ok(()) },
			(wrong, span) => Err(DiagBuilder2::error(format!("Expected {:?}, but found {:?} instead", expect, wrong)).span(span))
		}
	}

	fn require_reported(&mut self, expect: Token) -> ReportedResult<()> {
		match self.require(expect) {
			Ok(x) => Ok(x),
			Err(e) => {
				self.add_diag(e);
				Err(())
			}
		}
	}

	fn try_eat(&mut self, expect: Token) -> bool {
		match self.peek(0) {
			(actual, _) if actual == expect => { self.bump(); true },
			_ => false
		}
	}

	fn recover(&mut self, terminators: &[Token], eat_terminator: bool) {
		// println!("recovering to {:?}", terminators);
		loop {
			match self.peek(0) {
				(Eof, _) => return,
				(tkn, _) => {
					for t in terminators {
						if *t == tkn {
							if eat_terminator {
								self.bump();
							}
							return;
						}
					}
					self.bump();
				}
			}
		}
	}

	fn recover_balanced(&mut self, terminators: &[Token], eat_terminator: bool) {
		// println!("recovering (balanced) to {:?}", terminators);
		let mut stack = Vec::new();
		loop {
			let (tkn, sp) = self.peek(0);
			if stack.is_empty() {
				for t in terminators {
					if *t == tkn {
						if eat_terminator {
							self.bump();
						}
						return;
					}
				}
			}

			match tkn {
				OpenDelim(x) => stack.push(x),
				CloseDelim(x) => {
					if let Some(open) = stack.pop() {
						if open != x {
							self.add_diag(DiagBuilder2::error(format!("Found closing {:?} which is not the complement to the previous opening {:?}", x, open)).span(sp));
							break;
						}
					} else {
						self.add_diag(DiagBuilder2::error(format!("Found closing {:?} without an earlier opening {:?}", x, x)).span(sp));
						break;
					}
				}
				Eof => break,
				_ => (),
			}
			self.bump();
		}
	}
}


pub fn parse(input: Lexer) {
	let mut p = Parser::new(input);
	parse_source_text(&mut p);
	assert!(p.get_diagnostics().is_empty());
}

fn parse_source_text(p: &mut Parser) {
	// Parse the optional timeunits declaration.
	// TODO

	// Parse the descriptions in the source text.
	loop {
		let good = match p.peek(0) {
			(Keyword(Kw::Module), _) => {
				p.bump();
				match parse_module_decl(p) {
					Ok(_) => true,
					Err(_) => false,
				}
			}
			(Keyword(Kw::Interface),_) => {
				p.bump();
				match parse_interface_decl(p) {
					Ok(_) => true,
					Err(_) => false,
				}
			}
			(Eof,_) => break,
			(tkn,sp) => {
				p.add_diag(DiagBuilder2::fatal(format!("Expected top-level description, instead got `{:?}`", tkn)).span(sp));
				false
			}
		};

		// Recover by scanning forward to the next endmodule or endinterface.
		if !good {
			loop {
				match p.peek(0) {
					(Keyword(Kw::Endmodule), _) |
					(Keyword(Kw::Endinterface), _) => { p.bump(); break; },
					(Eof, _) => break,
					_ => p.bump(),
				}
			}
		}
	}
}



/// Convert a token to the corresponding lifetime. Yields `None` if the token
/// does not correspond to a lifetime.
fn as_lifetime(tkn: Token) -> Option<Lifetime> {
	match tkn {
		Keyword(Kw::Static) => Some(Lifetime::Static),
		Keyword(Kw::Automatic) => Some(Lifetime::Automatic),
		_ => None,
	}
}


fn parse_interface_decl(p: &mut Parser) -> ReportedResult<IntfDecl> {
	let mut span = p.last_span();

	// Eat the optional lifetime.
	let lifetime = match as_lifetime(p.peek(0).0) {
		Some(l) => { p.bump(); l },
		None => Lifetime::Static,
	};

	// Eat the interface name.
	let (name, name_sp) = p.eat_ident("interface name")?;
	println!("interface {}", name);

	// TODO: Parse package import declarations.

	// Eat the parameter port list.
	let param_ports = if p.try_eat(Hashtag) {
		parse_parameter_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the optional list of ports.
	let ports = if p.try_eat(OpenDelim(Paren)) {
		parse_port_list(p)?
	} else {
		Vec::new()
	};
	println!("interface {} has {} ports, {} param ports", name, ports.len(), param_ports.len());

	// Eat the semicolon at the end of the header.
	if !p.try_eat(Semicolon) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing semicolon \";\" after header of interface \"{}\"", name)).span(q));
	}

	// Eat the items in the interface.
	while p.peek(0).0 != Keyword(Kw::Endinterface) {
		match try_hierarchy_item(p) {
			Some(Ok(())) => (),
			Some(Err(())) => p.recover(&[Keyword(Kw::Endinterface)], false),
			None => {
				let (tkn, q) = p.peek(0);
				p.add_diag(DiagBuilder2::error(format!("Expected hierarchy item, got {:?}", tkn)).span(q));
				p.recover(&[Keyword(Kw::Endinterface)], false);
			}
		}
	}

	// Eat the endinterface keyword.
	if !p.try_eat(Keyword(Kw::Endinterface)) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing \"endinterface\" at the end of \"{}\"", name)).span(q));
	}

	span.expand(p.last_span());
	Ok(IntfDecl {
		span: span,
		lifetime: lifetime,
		name: name,
		name_span: name_sp,
		ports: ports,
	})
}


fn parse_parameter_port_list(p: &mut Parser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	p.require_reported(OpenDelim(Paren))?;

	while p.try_eat(Keyword(Kw::Parameter)) {
		// TODO: Parse data type or implicit type.

		// Eat the list of parameter assignments.
		loop {
			// parameter_identifier { unpacked_dimension } [ = constant_param_expression ]
			let (name, name_sp) = match p.eat_ident("parameter name") {
				Ok(x) => x,
				Err(()) => {
					p.recover_balanced(&[Comma, CloseDelim(Paren)], false);
					break;
				}
			};

			// TODO: Eat the unpacked dimensions.

			if p.try_eat(Assign) {
				match parse_constant_expr(p) {
					Ok(_) => (),
					Err(_) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
				}
			}

			v.push(());

			// Eat the trailing comma or closing parenthesis.
			match p.peek(0) {
				(Comma, sp) => {
					p.bump();
					match p.peek(0) {
						// The `parameter` keyword terminates this list of
						// assignments and introduces the next parameter.
						(Keyword(Kw::Parameter), _) => break,

						// A closing parenthesis indicates that the previous
						// comma was superfluous. Report the issue but continue
						// gracefully.
						(CloseDelim(Paren), _) => {
							// TODO: This should be an error in pedantic mode.
							p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
							break;
						}

						// All other tokens indicate the next assignment in the
						// list, so we just continue with the next iteration.
						_ => continue,
					}
				},
				(CloseDelim(Paren), _) => break,
				(_, sp) => {
					p.add_diag(DiagBuilder2::error("Expected , or ) after parameter assignment").span(sp));
					p.recover_balanced(&[CloseDelim(Paren)], false);
					break;
				}
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


fn parse_constant_expr(p: &mut Parser) -> ReportedResult<()> {
	let (tkn, span) = p.peek(0);

	// Try the unary operators.
	let unary_op = match tkn {
		Add =>  Some(()),
		Sub =>  Some(()),
		Not =>  Some(()),
		Neg =>  Some(()),
		And =>  Some(()),
		Nand => Some(()),
		Or =>   Some(()),
		Xor =>  Some(()),
		Nxor => Some(()),
		Xnor => Some(()),
		_ => None,
	};
	if let Some(x) = unary_op {
		p.bump();
		return parse_constant_expr(p);
	}

	// Parse the constant primary.
	let expr = match tkn {
		// Primary literals.
		UnsignedNumber(x) => { p.bump(); () },
		Literal(Str(x)) => { p.bump(); () },
		Literal(BasedInteger(size, signed, base, value)) => { p.bump(); () },
		Literal(UnbasedUnsized(x)) => { p.bump(); () },
		Ident(x) => { p.bump(); () },
		_ => {
			p.add_diag(DiagBuilder2::error("Expected constant primary expression").span(span));
			return Err(());
		}
	};

	// Try the binary operators.
	let (tkn, span) = p.peek(0);
	let binary_op = match tkn {
		Add =>  Some(()),
		Sub =>  Some(()),
		Mul =>  Some(()),
		Div =>  Some(()),
		Mod =>  Some(()),
		And =>  Some(()),
		Or =>  Some(()),
		Xor =>  Some(()),
		Xnor =>  Some(()),
		Nxor =>  Some(()),
		_ => None,
	};
	if let Some(x) = binary_op {
		p.bump();
		return parse_constant_expr(p);
	}

	// TODO: Parse ternary operator.

	Ok(())
}


/// Parse a module declaration, assuming that the leading `module` keyword has
/// already been consumed.
fn parse_module_decl(p: &mut Parser) -> ReportedResult<ModDecl> {
	let mut span = p.last_span();

	// Eat the optional lifetime.
	let lifetime = match as_lifetime(p.peek(0).0) {
		Some(l) => { p.bump(); l },
		None => Lifetime::Static,
	};

	// Eat the module name.
	let (name, name_sp) = p.eat_ident("module name")?;
	println!("module {}", name);

	// TODO: Parse package import declarations.

	// Eat the optional parameter port list.
	let param_ports = if p.try_eat(Hashtag) {
		parse_parameter_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the optional list of ports. Not having such a list requires the ports
	// to be defined further down in the body of the module.
	let ports = if p.try_eat(OpenDelim(Paren)) {
		parse_port_list(p)?
	} else {
		Vec::new()
	};
	println!("module {} has {} ports, {} param ports", name, ports.len(), param_ports.len());

	// Eat the semicolon after the header.
	if !p.try_eat(Semicolon) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing ; after header of module \"{}\"", name)).span(q));
	}

	// Eat the items in the module.
	while p.peek(0).0 != Keyword(Kw::Endmodule) && p.peek(0).0 != Eof {
		match try_hierarchy_item(p) {
			Some(Ok(())) => (),
			Some(Err(())) => p.recover(&[Keyword(Kw::Endmodule)], false),
			None => {
				let (tkn, q) = p.peek(0);
				p.add_diag(DiagBuilder2::error(format!("Expected hierarchy item, got {:?}", tkn)).span(q));
				p.recover(&[Keyword(Kw::Endmodule)], false);
			}
		}
	}

	// Eat the endmodule keyword.
	if !p.try_eat(Keyword(Kw::Endmodule)) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing \"endmodule\" at the end of \"{}\"", name)).span(q));
	}

	span.expand(p.last_span());
	Ok(ModDecl {
		span: span,
		lifetime: lifetime,
		name: name,
		name_span: name_sp,
		ports: ports,
	})
}


fn try_hierarchy_item(p: &mut Parser) -> Option<ReportedResult<()>> {
	// First attempt the simple cases where a keyword reliably identifies the
	// following item.
	let (tkn, _) = p.peek(0);
	let f = |p, func, term| Some(hierarchy_item_wrapper(p, func, term));
	let map_proc = |result: ReportedResult<Procedure>| Some(result.map(|r| {
		println!("parsed proc {:?}", r);
		()
	}));
	match tkn {
		Keyword(Kw::Localparam) => return f(p, parse_localparam_decl, Semicolon),
		Keyword(Kw::Parameter) => return f(p, parse_parameter_decl, Semicolon),
		Keyword(Kw::Modport) => return f(p, parse_modport_decl, Semicolon),

		// Structured procedures as per IEEE 1800-2009 section 9.2
		Keyword(Kw::Initial)     => return map_proc(parse_procedure(p, ProcedureKind::Initial)),
		Keyword(Kw::Always)      => return map_proc(parse_procedure(p, ProcedureKind::Always)),
		Keyword(Kw::AlwaysComb)  => return map_proc(parse_procedure(p, ProcedureKind::AlwaysComb)),
		Keyword(Kw::AlwaysLatch) => return map_proc(parse_procedure(p, ProcedureKind::AlwaysLatch)),
		Keyword(Kw::AlwaysFf)    => return map_proc(parse_procedure(p, ProcedureKind::AlwaysFf)),
		Keyword(Kw::Final)       => return map_proc(parse_procedure(p, ProcedureKind::Final)),
		Keyword(Kw::Function)    => return Some(parse_func_decl(p)),
		Keyword(Kw::Task)        => return Some(parse_task_decl(p)),

		_ => ()
	}

	// TODO: Handle the const and var keywords that may appear in front of a
	// data declaration, as well as the optional lifetime.

	// Now attempt to parse a data type or implicit type, which could introduce
	// and instantiation or data declaration. Due to the nature of implicit
	// types, a data declaration such as `foo[7:0];` would initially parse as an
	// explicit type `foo[7:0]`, and can only be identified as having an
	// implicit type when the semicolon is parsed. I.e. declarations that appear
	// to consist only of a type are actually declarations with an implicit
	// type.
	let ty = match parse_data_type(p) {
		Ok(x) => x,
		Err(_) => {
			p.recover_balanced(&[Semicolon], true);
			return Some(Err(()));
		}
	};

	// TODO: Handle the special case where the token following the parsed data
	// type is a [,;=], which indicates that the parsed type is actually a
	// variable declaration with implicit type (they look the same).

	// In case this is an instantiation, some parameter assignments may follow.
	if p.try_eat(Hashtag) {
		match parse_parameter_assignments(p) {
			Ok(x) => x,
			Err(()) => return Some(Err(()))
		};
	}

	// Parse the list of variable declaration assignments.
	loop {
		let (name, span) = match p.eat_ident_or("variable or instance name") {
			Ok(x) => x,
			Err(e) => {
				p.bump();
				p.add_diag(e);
				return Some(Err(()));
			}
		};

		// Parse the optional variable dimensions.
		let dims = match parse_optional_dimensions(p) {
			Ok(x) => x,
			Err(_) => return Some(Err(())),
		};

		// Parse the optional assignment.
		match p.peek(0) {
			(Assign, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Default variable assignments not implemented, for variable `{}`", name)).span(sp));
				p.recover_balanced(&[Comma, Semicolon], false);
			}
			(OpenDelim(Paren), sp) => {
				p.bump();
				match parse_list_of_port_connections(p) {
					Ok(x) => x,
					Err(()) => return Some(Err(())),
				};
				match p.require_reported(CloseDelim(Paren)) {
					Ok(_) => (),
					Err(x) => return Some(Err(x)),
				}
			}
			_ => ()
		}

		// Either parse the next variable declaration or break out of the loop
		// if we have encountered the semicolon that terminates the statement.
		match p.peek(0) {
			(Semicolon, _) => { p.bump(); break; },
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == Semicolon {
					// TODO: Make this an error in pedantic mode.
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					p.bump();
					break;
				} else {
					continue;
				}
			}
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ; after variable declaration").span(sp));
				p.recover(&[Semicolon], true);
				return Some(Err(()));
			}
		}
	}

	Some(Ok(()))
}


fn hierarchy_item_wrapper(p: &mut Parser, func: fn(&mut Parser) -> ReportedResult<()>, term: Token) -> ReportedResult<()> {
	p.bump();
	match func(p) {
		Ok(x) => {
			match p.require(Semicolon) {
				Err(d) => p.add_diag(d),
				_ => ()
			}
			Ok(x)
		}
		Err(e) => {
			p.recover(&[term], true);
			Err(e)
		}
	}
}


fn parse_localparam_decl(p: &mut Parser) -> ReportedResult<()> {
	// TODO: Parse data type or implicit type.

	// Eat the list of parameter assignments.
	loop {
		// parameter_identifier { unpacked_dimension } [ = constant_param_expression ]
		let (name, name_sp) = match p.eat_ident_or("parameter name") {
			Ok(x) => x,
			Err(e) => {
				p.add_diag(e);
				return Err(());
			}
		};

		// TODO: Eat the unpacked dimensions.

		// Eat the optional assignment.
		if p.try_eat(Assign) {
			match parse_constant_expr(p) {
				Ok(_) => (),
				Err(_) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
			}
		}

		// Eat the trailing comma or semicolon.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();

				// A closing parenthesis indicates that the previous
				// comma was superfluous. Report the issue but continue
				// gracefully.
				if p.peek(0).0 == Semicolon {
					// TODO: This should be an error in pedantic mode.
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(Semicolon, _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ; after parameter assignment, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	Ok(())
}


fn parse_parameter_decl(p: &mut Parser) -> ReportedResult<()> {
	let q = p.peek(0).1;
	p.add_diag(DiagBuilder2::error("Parameter declarations not implemented").span(q));
	Err(())
}


/// Parse a modport declaration.
///
/// ```
/// modport_decl: "modport" modport_item {"," modport_item} ";"
/// modport_item: ident "(" modport_ports_decl {"," modport_ports_decl} ")"
/// modport_ports_decl:
///   port_direction modport_simple_port {"," modport_simple_port} |
///   ("import"|"export") modport_tf_port {"," modport_tf_port} |
///   "clocking" ident
/// modport_simple_port: ident | "." ident "(" [expr] ")"
/// ```
fn parse_modport_decl(p: &mut Parser) -> ReportedResult<()> {
	loop {
		parse_modport_item(p)?;
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (Semicolon, _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			},
			(Semicolon, _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ; after modport declaration, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	Ok(())
}


fn parse_modport_item(p: &mut Parser) -> ReportedResult<()> {
	let (name, span) = match p.eat_ident_or("modport name") {
		Ok(x) => x,
		Err(e) => {
			p.add_diag(e);
			return Err(());
		}
	};
	println!("modport {}", name);

	// Eat the opening parenthesis.
	if !p.try_eat(OpenDelim(Paren)) {
		let (tkn, q) = p.peek(0);
		p.add_diag(DiagBuilder2::error(format!("Expected ( after modport name `{}`, got `{:?}`", name, tkn)).span(q));
		return Err(());
	}

	// Parse the port declarations.
	loop {
		match parse_modport_port_decl(p) {
			Ok(x) => x,
			Err(_) => {
				p.recover(&[CloseDelim(Paren)], true);
				return Err(());
			}
		}
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (CloseDelim(Paren), _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			}
			(CloseDelim(Paren), _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ) after port declaration, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	// Eat the closing parenthesis.
	if !p.try_eat(CloseDelim(Paren)) {
		let (tkn, q) = p.peek(0);
		p.add_diag(DiagBuilder2::error(format!("Expected ) after port list of modport `{}`, got `{:?}`", name, tkn)).span(q));
		return Err(());
	}

	Ok(())
}


/// ```
/// modport_ports_decl:
///   port_direction modport_simple_port {"," modport_simple_port} |
///   ("import"|"export") modport_tf_port {"," modport_tf_port} |
///   "clocking" ident
/// modport_simple_port: ident | "." ident "(" [expr] ")"
/// ```
fn parse_modport_port_decl(p: &mut Parser) -> ReportedResult<()> {
	let (tkn, span) = p.peek(0);

	// Attempt to parse a simple port introduced by one of the port direction
	// keywords.
	if let Some(dir) = as_port_direction(tkn) {
		p.bump();
		loop {
			if p.try_eat(Period) {
				let (name, span) = p.eat_ident("port name")?;
				p.require_reported(OpenDelim(Paren))?;
				// TODO: Parse expression.
				p.require_reported(CloseDelim(Paren))?;
			} else {
				let (name, span) = p.eat_ident("port_name")?;
			}

			// Decide whether we should continue iterating and thus consuming
			// more simple ports. According to the grammar, a comma followed by
			// a keyword indicates a different port declaration, so we abort.
			// Otherwise, if the next item is a comma still, we continue
			// iteration. In all other cases, we assume the port declaration to
			// be done.
			match (p.peek(0).0, p.peek(1).0) {
				(Comma, Keyword(_)) => break,
				(Comma, _) => {
					p.bump();
					continue;
				},
				_ => break,
			}
		}
		return Ok(());
	}

	// TODO: Parse modport_tf_port.

	// Attempt to parse a clocking declaration.
	if p.try_eat(Keyword(Kw::Clocking)) {
		// TODO: Parse modport_clocking_declaration.
		p.add_diag(DiagBuilder2::error("modport clocking declaration not implemented").span(span));
		return Err(());
	}

	// If we've come thus far, none of the above matched.
	p.add_diag(DiagBuilder2::error("Expected port declaration").span(span));
	Err(())
}

/// Convert a token to the corresponding PortDir. The token may be one of the
/// keywords `input`, `output`, `inout`, or `ref`. Otherwise `None` is returned.
fn as_port_direction(tkn: Token) -> Option<PortDir> {
	match tkn {
		Keyword(Kw::Input) => Some(PortDir::Input),
		Keyword(Kw::Output) => Some(PortDir::Output),
		Keyword(Kw::Inout) => Some(PortDir::Inout),
		Keyword(Kw::Ref) => Some(PortDir::Ref),
		_ => None,
	}
}


/// Parse a data type.
fn parse_data_type(p: &mut Parser) -> ReportedResult<Type> {
	use svlog::ast::*;

	// Decide what general type this is.
	let (tkn, mut span) = p.peek(0);
	let data = {
		match tkn {
			Keyword(kw) => {
				match kw {
					// Integer Vector Types
					Kw::Bit => BitType,
					Kw::Logic => LogicType,
					Kw::Reg => RegType,

					// Integer Atom Types
					Kw::Byte => ByteType,
					Kw::Shortint => ShortIntType,
					Kw::Int => IntType,
					Kw::Longint => LongIntType,
					Kw::Integer => IntType,
					Kw::Time => TimeType,

					e => {
						p.add_diag(DiagBuilder2::error(format!("Expected data type, found keyword {:?}", kw)).span(span));
						return Err(());
					}
				}
			},
			Ident(n) | EscIdent(n) => NamedType(n),
			_ => ImplicitType,
		}
	};
	if data != ImplicitType {
		p.bump();
	}

	// Parse the optional sign information.
	let sign = match p.peek(0) {
		(Keyword(Kw::Signed), q) => { span.expand(q); p.bump(); TypeSign::Signed },
		(Keyword(Kw::Unsigned), q) => { span.expand(q); p.bump(); TypeSign::Unsigned },
		_ => TypeSign::None
	};

	// Parse the optional dimensions.
	let (dims, dims_span) = parse_optional_dimensions(p)?;
	if !dims.is_empty() {
		span.expand(dims_span);
	}

	Ok(Type {
		span: span,
		data: data,
		sign: sign,
		dims: dims,
	})
}


fn parse_optional_dimensions(p: &mut Parser) -> ReportedResult<(Vec<TypeDim>, Span)> {
	let mut v = Vec::new();
	let mut span;
	if let Some((d,sp)) = try_dimension(p)? {
		span = sp;
		v.push(d);
	} else {
		return Ok((v, INVALID_SPAN));
	}
	while let Some((d,sp)) = try_dimension(p)? {
		v.push(d);
		span.expand(sp);
	}
	Ok((v, span))
}


fn try_dimension(p: &mut Parser) -> ReportedResult<Option<(TypeDim, Span)>> {
	// Eat the leading opening brackets.
	if !p.try_eat(OpenDelim(Brack)) {
		return Ok(None);
	}
	let mut span = p.last_span();

	let dim = match p.peek(0).0 {
		CloseDelim(Brack) => {
			p.bump();
			TypeDim::Unsized
		},
		Mul => {
			p.bump();
			TypeDim::Associative
		},
		// TODO: Handle the queue case [$] and [$:<const_expr>]
		_ => {
			// What's left must either be a single constant expression, or a range
			// consisting of two constant expressions.
			let expr = match parse_constant_expr(p) {
				Ok(x) => x,
				Err(_) => {
					p.recover_balanced(&[CloseDelim(Brack)], true);
					return Err(());
				}
			};

			// If the expression is followed by a colon `:`, this is a constant range
			// rather than a constant expression.
			if p.try_eat(Colon) {
				let other = match parse_constant_expr(p) {
					Ok(x) => x,
					Err(_) => {
						p.recover_balanced(&[CloseDelim(Brack)], true);
						return Err(());
					}
				};
				TypeDim::Range
			} else {
				TypeDim::Expr
			}
		}
	};

	// Eat the closing brackets.
	match p.peek(0) {
		(CloseDelim(Brack), sp) => {
			span.expand(sp);
			p.bump();
			return Ok(Some((dim, span)));
		},
		(tkn, sp) => {
			p.add_diag(DiagBuilder2::error(format!("Expected closing brackets `]` after dimension, got {:?}", tkn)).span(sp));
			return Err(());
		}
	}
}


fn parse_list_of_port_connections(p: &mut Parser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	if p.peek(0).0 == CloseDelim(Paren) {
		return Ok(v);
	}
	loop {
		if p.try_eat(Period) {
			if p.try_eat(Mul) {
				// handle .* case
				let q = p.last_span();
				p.add_diag(DiagBuilder2::error("Don't know how to handle .* port connections").span(q));
			} else {
				let (name, name_sp) = p.eat_ident("port name")?;
				// handle .name, .name(), and .name(expr) cases
				if p.try_eat(OpenDelim(Paren)) {
					if !p.try_eat(CloseDelim(Paren)) {
						match parse_expr(p) {
							Ok(_) => (),
							Err(x) => {
								p.recover_balanced(&[CloseDelim(Paren)], false);
							},
						}
						p.require_reported(CloseDelim(Paren))?;
					}
				}
			}
		} else {
			// handle expr
			parse_expr(p)?;
		}

		// Depending on the next character, continue with the next port
		// connection or close the loop.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (CloseDelim(Paren), _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			}
			(CloseDelim(Paren), _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ) after list of port connections, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	Ok(v)
}


fn parse_expr(p: &mut Parser) -> ReportedResult<()> {
	parse_expr_prec(p, Precedence::Min)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Precedence {
	Min,
	Concatenation,
	Assignment,
	Implication,
	Ternary,
	LogicOr,
	LogicAnd,
	BinOr,
	BinXor,
	BinAnd,
	Equality,
	Relational,
	Shift,
	Add,
	Mul,
	Pow,
	Unary,
	Scope,
	Max,
}

fn parse_expr_prec(p: &mut Parser, precedence: Precedence) -> ReportedResult<()> {
	let prefix = parse_expr_first(p, precedence)?;

	// Try to parse the index and call expressions.
	let (tkn, sp) = p.peek(0);
	match tkn {
		// Index: "[" range_expression "]"
		OpenDelim(Brack) if precedence <= Precedence::Scope => {
			p.bump();
			match parse_range_expr(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Brack)], true);
					return Err(e);
				}
			}
			p.require_reported(CloseDelim(Brack))?;
			return Ok(());
		}

		// Call: "(" [list_of_arguments] ")"
		OpenDelim(Paren) if precedence <= Precedence::Scope => {
			p.bump();
			p.add_diag(DiagBuilder2::error("Don't know how to handle call expressions").span(sp));
			p.recover_balanced(&[CloseDelim(Paren)], true);
			return Ok(());
		}

		Period if precedence <= Precedence::Scope => {
			p.bump();
			p.eat_ident("member name")?;
			return Ok(());
		}

		Namespace if precedence <= Precedence::Scope => {
			p.bump();
			p.eat_ident("scope name")?;
			return Ok(());
		}

		Inc if precedence <= Precedence::Unary => {
			p.bump();
			return Ok(());
		}

		Dec if precedence <= Precedence::Unary => {
			p.bump();
			return Ok(());
		}

		_ => ()
	}

	// Try to parse binary operations.
	if let Some(op) = as_binary_operator(tkn) {
		let prec = op.get_precedence();
		if precedence <= prec {
			p.bump();
			parse_expr_prec(p, prec)?;
			return Ok(());
		}
	}

	Ok(prefix)
}

fn parse_expr_first(p: &mut Parser, precedence: Precedence) -> ReportedResult<()> {
	// Certain expressions are introduced by an operator or keyword. Handle
	// these cases first, since they are the quickest to decide.
	match p.peek(0) {
		(Inc, _) if precedence <= Precedence::Unary => {
			p.bump();
			parse_expr_prec(p, Precedence::Unary)?;
			return Ok(());
		}

		(Dec, _) if precedence <= Precedence::Unary => {
			p.bump();
			parse_expr_prec(p, Precedence::Unary)?;
			return Ok(());
		}

		(Keyword(Kw::Tagged), sp) => {
			p.add_diag(DiagBuilder2::error("Tagged union expressions not implemented").span(sp));
			return Err(());
		}

		_ => ()
	}

	// Try the unary operators next.
	if let Some(op) = as_unary_operator(p.peek(0).0) {
		p.bump();
		parse_primary_expr(p)?;
		return Ok(());
	}

	// Since none of the above matched, this must be a primary expression.
	parse_primary_expr(p)
}


fn parse_primary_expr(p: &mut Parser) -> ReportedResult<()> {
	let (tkn, sp) = p.peek(0);
	match tkn {
		// Primary Literals
		UnsignedNumber(_) => { p.bump(); return Ok(()); }
		Literal(Lit::Str(..)) => { p.bump(); return Ok(()); }
		Literal(Lit::Decimal(..)) => { p.bump(); return Ok(()); }
		Literal(Lit::BasedInteger(..)) => { p.bump(); return Ok(()); }
		Literal(Lit::UnbasedUnsized(..)) => { p.bump(); return Ok(()); }

		// Identifiers
		Ident(_) => { p.bump(); return Ok(()); }
		EscIdent(_) => { p.bump(); return Ok(()); }
		SysIdent(_) => { p.bump(); return Ok(()); }

		// Concatenation and empty queue
		OpenDelim(Brace) => {
			p.bump();
			if p.try_eat(CloseDelim(Brace)) {
				// TODO: Handle empty queue.
				p.add_diag(DiagBuilder2::error("Don't know what to do with an empty queue").span(sp));
				return Ok(());
			}
			match parse_concat_expr(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Brace)], true);
					return Err(e);
				}
			};
			p.require_reported(CloseDelim(Brace))?;
			return Ok(());
		}

		// Parenthesis
		OpenDelim(Paren) => {
			p.bump();
			match parse_primary_parenthesis(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Paren)], true);
					return Err(e);
				}
			};
			p.require_reported(CloseDelim(Paren))?;
			return Ok(());
		}

		_ => {
			p.add_diag(DiagBuilder2::error("Expected primary expression").span(sp));
			return Err(());
		}
	}
}


pub enum StreamDir {
	In,
	Out,
}

fn parse_concat_expr(p: &mut Parser) -> ReportedResult<()> {
	/// Streaming concatenations have a "<<" or ">>" following the opening "{".
	let stream = match p.peek(0).0 {
		Shl => Some(StreamDir::Out),
		Shr => Some(StreamDir::In),
		_ => None
	};

	if let Some(dir) = stream {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Don't know how to handle streaming concatenation").span(q));
		return Err(());
	}

	// Parse the expression that follows the opening "{". Depending on whether
	// this is a regular concatenation or a multiple concatenation, the meaning
	// of the expression changes.
	let first_expr = parse_expr_prec(p, Precedence::Concatenation)?;

	// If the expression is followed by a "{", this is a multiple concatenation.
	if p.try_eat(OpenDelim(Brace)) {
		match parse_expr_list(p) {
			Ok(x) => x,
			Err(e) => {
				p.recover_balanced(&[CloseDelim(Brace)], true);
				return Err(e);
			}
		};
		p.require_reported(CloseDelim(Brace))?;
		return Ok(());
	}

	// Otherwise this is just a regular concatenation, so the first expression
	// may be followed by "," and another expression multiple times.
	while p.try_eat(Comma) {
		if p.peek(0).0 == CloseDelim(Brace) {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(q));
			break;
		}
		parse_expr_prec(p, Precedence::Max)?;
	}

	Ok(())
}


fn parse_expr_list(p: &mut Parser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	loop {
		v.push(parse_expr_prec(p, Precedence::Max)?);

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Brace) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Brace), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or } after expression").span(sp));
				return Err(());
			}
		}
	}
	Ok(v)
}


/// Parse the tail of a primary expression that started with a parenthesis.
///
/// ## Syntax
/// ```
/// "(" expression ")"
/// "(" expression ":" expression ":" expression ")"
/// ```
fn parse_primary_parenthesis(p: &mut Parser) -> ReportedResult<()> {
	parse_expr_prec(p, Precedence::Scope)?;
	if p.try_eat(Colon) {
		parse_expr_prec(p, Precedence::Scope)?;
		p.require_reported(Colon)?;
		parse_expr_prec(p, Precedence::Scope)?;
	}
	return Ok(());
}


/// Parse a range expression.
///
/// ## Syntax
/// ```
/// expression
/// expression ":" expression
/// expression "+:" expression
/// expression "-:" expression
/// ```
fn parse_range_expr(p: &mut Parser) -> ReportedResult<()> {
	let first_expr = parse_expr(p)?;

	match p.peek(0).0 {
		Colon => {
			p.bump();
			parse_expr(p)?;
			return Ok(());
		}

		AddColon => {
			p.bump();
			parse_expr(p)?;
			return Ok(());
		}

		SubColon => {
			p.bump();
			parse_expr(p)?;
			return Ok(());
		}

		// Otherwise the index expression consists only of one expression.
		_ => {
			return Ok(());
		}
	}
}


#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum UnaryOp {
	Add,
	Sub,
	Stuff,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum BinaryOp {
	Add,
	Sub,
	Mul,
	Div,
	Mod,
	Shl,
	Shr,
	Pow,
	Stuff,
}

impl BinaryOp {
	fn get_precedence(self) -> Precedence {
		use self::BinaryOp::*;
		match self {
			Add | Sub => Precedence::Add,
			Mul | Div | Mod => Precedence::Mul,
			Shl | Shr => Precedence::Shift,
			Pow => Precedence::Pow,
			Stuff => Precedence::Add,
		}
	}
}

/// Convert a token to the corresponding UnaryOp. Return `None` if the token
/// does not map to a unary operator.
fn as_unary_operator(tkn: Token) -> Option<UnaryOp> {
	match tkn {
		Add => Some(UnaryOp::Add),
		Sub => Some(UnaryOp::Sub),
		Not => Some(UnaryOp::Stuff),
		Neg => Some(UnaryOp::Stuff),
		And => Some(UnaryOp::Stuff),
		Nand => Some(UnaryOp::Stuff),
		Or => Some(UnaryOp::Stuff),
		Nor => Some(UnaryOp::Stuff),
		Xor => Some(UnaryOp::Stuff),
		Nxor => Some(UnaryOp::Stuff),
		Xnor => Some(UnaryOp::Stuff),
		_ => None,
	}
}

/// Convert a token to the corresponding BinaryOp. Return `None` if the token
/// does not map to a binary operator.
fn as_binary_operator(tkn: Token) -> Option<BinaryOp> {
	match tkn {
		Add => Some(BinaryOp::Stuff),
		Sub => Some(BinaryOp::Stuff),
		Mul => Some(BinaryOp::Stuff),
		Div => Some(BinaryOp::Stuff),
		Mod => Some(BinaryOp::Stuff),
		Shl => Some(BinaryOp::Stuff),
		Shr => Some(BinaryOp::Stuff),
		Pow => Some(BinaryOp::Stuff),
		Rarrow => Some(BinaryOp::Stuff),
		_ => None,
	}
}


/// Parse a comma-separated list of ports, up to a closing parenthesis. Assumes
/// that the opening parenthesis has already been consumed.
fn parse_port_list(p: &mut Parser) -> ReportedResult<Vec<Port>> {
	let mut v = Vec::new();

	// In case the port list is empty.
	if p.try_eat(CloseDelim(Paren)) {
		return Ok(v);
	}

	loop {
		// Parse a port.
		match parse_port(p, v.last()) {
			Ok(x) => v.push(x),
			Err(()) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
		}

		// Depending on what follows, continue or break out of the loop.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Paren) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Paren), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ) after port").span(sp));
				p.recover_balanced(&[CloseDelim(Paren)], false);
				break;
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


/// Parse one port in a module or interface port list. The `prev` argument shall
/// be a reference to the previously parsed port, or `None` if this is the first
/// port in the list. This is required since ports inherit certain information
/// from their predecessor if omitted.
fn parse_port(p: &mut Parser, prev: Option<&Port>) -> ReportedResult<Port> {
	let mut span = p.peek(0).1;

	// Consume the optional port direction.
	let mut dir = as_port_direction(p.peek(0).0);
	if dir.is_some() {
		p.bump();
	}

	// Consume the optional net type or var keyword, which determines the port
	// kind.
	let mut kind = match p.peek(0).0 {
		// Net Types
		Keyword(Kw::Supply0) => Some(NetPort),
		Keyword(Kw::Supply1) => Some(NetPort),
		Keyword(Kw::Tri)     => Some(NetPort),
		Keyword(Kw::Triand)  => Some(NetPort),
		Keyword(Kw::Trior)   => Some(NetPort),
		Keyword(Kw::Trireg)  => Some(NetPort),
		Keyword(Kw::Tri0)    => Some(NetPort),
		Keyword(Kw::Tri1)    => Some(NetPort),
		Keyword(Kw::Uwire)   => Some(NetPort),
		Keyword(Kw::Wire)    => Some(NetPort),
		Keyword(Kw::Wand)    => Some(NetPort),
		Keyword(Kw::Wor)     => Some(NetPort),

		// Var Kind
		Keyword(Kw::Var)     => Some(VarPort),
		_ => None
	};
	if kind.is_some() {
		p.bump();
	}

	// Try to parse ports of the form:
	// "." port_identifier "(" [expression] ")"
	if p.try_eat(Period) {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Ports starting with a . not yet supported").span(q));
		return Err(())
	}

	// Otherwise parse the port data type, which may be a whole host of
	// different things.
	let mut ty = Some(parse_data_type(p)?);

	// Here goes the tricky part: If the data type not followed by the name (and
	// optional dimensions) of the port, the data type actually was the port
	// name. These are indistinguishable.
	let (name, name_span, (dims, dims_span)) = if let Some((name, span)) = p.try_eat_ident() {
		(name, span, parse_optional_dimensions(p)?)
	} else {
		// TODO: Extract name and dimensions from data type and change type to
		// None.
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Ports with implicit data types not yet supported").span(q));
		return Err(());
	};

	// Determine the kind of the port based on the optional kind keywords, the
	// direction, and the type.
	if dir.is_none() && kind.is_none() && ty.is_none() && prev.is_some() {
		dir = Some(prev.unwrap().dir.clone());
		kind = Some(prev.unwrap().kind.clone());
		ty = Some(prev.unwrap().ty.clone());
	} else {
		// The direction defaults to inout.
		if dir.is_none() {
			dir = Some(PortDir::Inout);
		}

		// The type defaults to logic.
		if ty.is_none() {
			ty = Some(Type {
				span: INVALID_SPAN,
				data: LogicType,
				sign: TypeSign::None,
				dims: Vec::new(),
			});
		}

		// The kind defaults to different things based on the direction and
		// type:
		// - input,inout: default net
		// - ref: var
		// - output (implicit type): net
		// - output (explicit type): var
		if kind.is_none() {
			kind = Some(match dir.unwrap() {
				PortDir::Input | PortDir::Inout => NetPort,
				PortDir::Ref => VarPort,
				PortDir::Output if ty.clone().unwrap().data == ImplicitType => NetPort,
				PortDir::Output => VarPort,
			});
		}
	}

	// Parse the optional initial assignment for this port.
	if p.try_eat(Equal) {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Ports with initial assignment not yet supported").span(q));
	}

	// Update the port's span to cover all of the tokens consumed.
	span.expand(p.last_span());

	Ok(Port {
		span: span,
		name: name,
		name_span: name_span,
		kind: kind.unwrap(),
		ty: ty.unwrap(),
		dir: dir.unwrap(),
		dims: dims,
	})
}


fn parse_parameter_assignments(p: &mut Parser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	p.require_reported(OpenDelim(Paren))?;

	// In case there are no parameter assignments, the opening parenthesis is
	// directly followed by a closing one.
	if p.try_eat(CloseDelim(Paren)) {
		return Ok(v);
	}

	loop {
		match parse_parameter_assignment(p) {
			Ok(x) => v.push(x),
			Err(()) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
		}

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Paren) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Paren), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ) after parameter assignment, found").span(sp));
				p.recover_balanced(&[CloseDelim(Paren)], false);
				break;
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


fn parse_parameter_assignment(p: &mut Parser) -> ReportedResult<()> {
	// If the parameter assignment starts with a ".", this is a named
	// assignment. Otherwise it's an ordered assignment.
	if p.try_eat(Period) {
		let (name, name_span) = p.eat_ident("parameter name")?;
		p.require_reported(OpenDelim(Paren))?;
		let expr = match parse_expr(p) {
			Ok(x) => x,
			Err(()) => {
				p.recover_balanced(&[CloseDelim(Paren)], true);
				return Err(());
			}
		};
		p.require_reported(CloseDelim(Paren))?;
		// println!("named param assignment: {} = {:?}", name, expr);
		Ok(())
	} else {
		let expr = parse_expr(p)?;
		// println!("ordered param assignment: {:?}", expr);
		Ok(())
	}
}


fn parse_procedure(p: &mut Parser, kind: ProcedureKind) -> ReportedResult<Procedure> {
	p.bump();
	let mut span = p.last_span();
	let stmt = parse_stmt(p)?;
	span.expand(p.last_span());
	Ok(Procedure {
		span: span,
		kind: kind,
		stmt: stmt,
	})
}


fn parse_func_decl(p: &mut Parser) -> ReportedResult<()> {
	let q = p.peek(0).1;
	p.bump();
	p.add_diag(DiagBuilder2::error("Don't know how to parse function declarations").span(q));
	p.recover_balanced(&[Keyword(Kw::Endfunction)], true);
	Err(())
}


fn parse_task_decl(p: &mut Parser) -> ReportedResult<()> {
	let q = p.peek(0).1;
	p.bump();
	p.add_diag(DiagBuilder2::error("Don't know how to parse task declarations").span(q));
	p.recover_balanced(&[Keyword(Kw::Endtask)], true);
	Err(())
}


fn parse_stmt(p: &mut Parser) -> ReportedResult<Stmt> {
	let mut span = p.peek(0).1;

	// Null statements simply consist of a semicolon.
	if p.try_eat(Semicolon) {
		return Ok(Stmt::new_null(span));
	}

	// Consume the optional statement label.
	let mut label = if p.peek(1).0 == Colon {
		let (n,_) = p.eat_ident("statement label")?;
		p.bump(); // eat the colon
		Some(n)
	} else {
		None
	};

	// Parse the actual statement item.
	let (tkn, sp) = p.peek(0);
	let data = match tkn {
		// Sequential blocks
		OpenDelim(Bgend) => {
			p.bump();
			let (stmts, _) = parse_block(p, &mut label, &[CloseDelim(Bgend)])?;
			SequentialBlock(stmts)
		}

		// Parallel blocks
		Keyword(Kw::Fork) => {
			p.bump();
			let (stmts, terminator) = parse_block(p, &mut label, &[Keyword(Kw::Join), Keyword(Kw::JoinAny), Keyword(Kw::JoinNone)])?;
			let join = match terminator {
				Keyword(Kw::Join) => JoinKind::All,
				Keyword(Kw::JoinAny) => JoinKind::Any,
				Keyword(Kw::JoinNone) => JoinKind::None,
				x => panic!("Invalid parallel block terminator {:?}", x),
			};
			ParallelBlock(stmts, join)
		}

		x => {
			p.add_diag(DiagBuilder2::error(format!("Expected statement, got {:?} instead", x)).span(sp));
			p.recover_balanced(&[Semicolon], true);
			return Err(());
		}
	};
	span.expand(p.last_span());

	Ok(Stmt {
		span: span,
		label: label,
		data: data,
	})
}


fn parse_block(p: &mut Parser, label: &mut Option<Name>, terminators: &[Token]) -> ReportedResult<(Vec<Stmt>, Token)> {
	let span = p.last_span();

	// Consume the optional block label. If the block has already been labelled
	// via a statement label, an additional block label is illegal.
	if p.try_eat(Colon) {
		let (name, name_span) = p.eat_ident("block label")?;
		if let Some(existing) = *label {
			if name == existing {
				p.add_diag(DiagBuilder2::warning(format!("Block {} labelled twice", name)).span(name_span));
			} else {
				p.add_diag(DiagBuilder2::error(format!("Block has been given two conflicting labels, {} and {}", existing, name)).span(name_span));
			}
		} else {
			*label = Some(name);
		}
	}

	// Parse the block statements.
	let mut v = Vec::new();
	let terminator;
	'outer: loop {
		// Check if we have reached one of the terminators.
		let tkn = p.peek(0).0;
		for term in terminators {
			if tkn == *term {
				terminator = *term;
				break 'outer;
			}
		}

		// Otherwise parse the next statement.
		match parse_stmt(p) {
			Ok(x) => v.push(x),
			Err(()) => {
				p.recover_balanced(terminators, false);
				terminator = p.peek(0).0;
				p.bump();
				break;
			}
		}
	}

	// Consume the optional block label after the terminator and verify that it
	// matches the label provided at the beginning of the block.
	if p.try_eat(Colon) {
		let (name, name_span) = p.eat_ident("block label")?;
		if let Some(before) = *label {
			if before != name {
				p.add_diag(DiagBuilder2::error(format!("Block label {} at end of block does not match label {} at beginning of block", name, before)).span(name_span));
			}
		} else {
			p.add_diag(DiagBuilder2::error(format!("Block label {} provided at the end of the block, but not at the beginning", name)).span(name_span));
		}
	}

	Ok((v, terminator))
}



#[cfg(test)]
mod tests {
	use source::*;
	use name::*;
	use svlog::preproc::*;
	use svlog::lexer::*;

	fn parse(input: &str) {
		use std::cell::Cell;
		thread_local!(static INDEX: Cell<usize> = Cell::new(0));
		let sm = get_source_manager();
		let idx = INDEX.with(|i| {
			let v = i.get();
			i.set(v+1);
			v
		});
		let source = sm.add(&format!("test_{}.sv", idx), input);
		let pp = Preprocessor::new(source, &[]);
		let lexer = Lexer::new(pp);
		super::parse(lexer);
	}

	#[test]
	fn intf_empty() {
		parse("interface Foo; endinterface");
	}

	#[test]
	fn intf_params() {
		parse("interface Foo #(); endinterface");
		parse("interface Foo #(parameter bar = 32); endinterface");
		parse("interface Foo #(parameter bar = 32, baz = 64); endinterface");
		parse("interface Foo #(parameter bar = 32, parameter baz = 64); endinterface");
	}

	#[test]
	fn intf_header() {
		// parse("interface Foo ();")
	}
}
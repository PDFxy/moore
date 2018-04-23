// Copyright (c) 2018 Fabian Schuiki

//! Multi-type arena allocation

#![deny(missing_docs)]

use std::borrow::Cow;

/// Allocates objects into an arena.
pub trait Alloc<T> {
    /// Allocate an object of type `T` into this arena.
    fn alloc(&self, value: T) -> &mut T;
}

/// Allocates objects into a remote arena.
///
/// In contrast to `Alloc`, the lifetime of the references returned by this
/// trait are not bound to the trait itself. Rather, the lifetime is a parameter
/// of the trait itself. This allows context objects to be created that hold a
/// reference to an arena, and allow for direct allocation into that arena.
pub trait AllocInto<'t, T> {
    /// Allocate an object of type `T`.
    fn alloc(&self, value: T) -> &'t mut T;
}

impl<'t, T> AllocInto<'t, T> for &'t Alloc<T> {
    fn alloc(&self, value: T) -> &'t mut T {
        Alloc::alloc(*self, value)
    }
}

/// Allocates objects into an arena.
pub trait Allok<'a, 't, T> {
    /// Allocate an object of type `T` into this arena.
    fn allok(&'a self, value: T) -> &'t mut T;
}

/// Allocates objects into an arena.
pub trait AllokOwned<'a, 't, T: ToOwned + ?Sized> {
    /// Allocate an object of type `T` into this arena.
    fn allok_owned(&'a self, value: <T as ToOwned>::Owned) -> &'t mut T;
}

impl<'a, 't, T: ToOwned<Owned = T>> AllokOwned<'a, 't, T> for Allok<'a, 't, T> {
    fn allok_owned(&'a self, value: T) -> &'t mut T {
        self.allok(value)
    }
}

/// Allocates objects into an arena.
pub trait AllokCow<'a, 't, T: ToOwned + ?Sized>: AllokOwned<'a, 't, T> {

    /// Conditionally allocate a CoW object of type `T`.
    fn maybe_allok(&'a self, value: Cow<'t, T>) -> &'t T {
        match value {
            Cow::Borrowed(x) => x,
            Cow::Owned(x) => self.allok_owned(x),
        }
    }

    /// Forcefully allocate a CoW object of type `T`.
    fn force_allok(&'a self, value: Cow<T>) -> &'t T {
        self.allok_owned(value.into_owned())
    }
}

/// Generate a collection of arenas for different types.
#[macro_export]
macro_rules! make_arenas {
    ($(#[$arena_attr:meta])* pub struct $arena_name:ident { $($name:ident: $type:ty,)* }) => {
        make_arenas!{ IMPL $($arena_attr),*; $arena_name; []; $($name: $type,)* }
    };

    ($(#[$arena_attr:meta])* pub struct $arena_name:ident<$($lt:tt),+> { $($name:ident: $type:ty,)* }) => {
        make_arenas!{ IMPL $($arena_attr),*; $arena_name; [$($lt),+]; $($name: $type,)* }
    };

    (IMPL $($arena_attr:meta),*; $arena_name:ident; [$($lt:tt),*]; $($name:ident: $type:ty,)*) => {
        $(#[$arena_attr])*
        #[allow(missing_docs)]
        pub struct $arena_name<$($lt),*> {
            $(pub $name: ::typed_arena::Arena<$type>,)*
        }

        make_arenas!(STRUCT_IMPL $arena_name; [$($lt),*]; $($name: $type,)*);
    };

    (STRUCT_IMPL $arena_name:ident; [$($lt:tt),*]; $($name:ident: $type:ty,)*) => {
        impl<$($lt),*> $arena_name<$($lt),*> {
            /// Create a new arena.
            pub fn new() -> $arena_name<$($lt),*> {
                $arena_name {
                    $($name: ::typed_arena::Arena::new(),)*
                }
            }
        }

        impl<$($lt),*> Default for $arena_name<$($lt),*> {
            fn default() -> $arena_name<$($lt),*> {
                $arena_name::new()
            }
        }

        make_arenas!(TRAIT_IMPL $arena_name; [$($lt),*]; $($name: $type,)*);
    };

    (TRAIT_IMPL $arena_name:ident; [$($lt:tt),*]; $name:ident: $type:ty, $($tail_name:ident: $tail_type:ty,)*) => {
        impl<$($lt),*> $crate::arenas::Alloc<$type> for $arena_name<$($lt),*> {
            fn alloc(&self, value: $type) -> &mut $type {
                self.$name.alloc(value)
            }
        }

        impl<'a, $($lt),*> $crate::arenas::Allok<'a, 'a, $type> for $arena_name<$($lt),*> where $($lt: 'a),* {
            fn allok(&'a self, value: $type) -> &'a mut $type {
                self.$name.alloc(value)
            }
        }

        make_arenas!(TRAIT_IMPL $arena_name; [$($lt),*]; $($tail_name: $tail_type,)*);
    };

    (TRAIT_IMPL $arena_name:ident; [$($lt:tt),*];) => {}
}

// Copyright 2018 ETH Zurich and University of Bologna.
//
// Copyright and related rights are licensed under the Solderpad Hardware
// License, Version 0.51 (the "License"); you may not use this file except in
// compliance with the License. You may obtain a copy of the License at
// http://solderpad.org/licenses/SHL-0.51. Unless required by applicable law
// or agreed to in writing, software, hardware and materials distributed under
// this License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.
//
// Fabian Schuiki <fschuiki@iis.ee.ethz.ch>

/// A binary to gray code converter.
module binary_to_gray #(
    parameter int N = 8
)(
    input  logic [N-1:0] A,
    output logic [N-1:0] Z
);
    assign Z = A ^ (A >> 1);
endmodule

/// A gray code to binary converter.
module gray_to_binary #(
    parameter int N = 8
)(
    input  logic [N-1:0] A,
    output logic [N-1:0] Z
);
    for (genvar i = 0; i < N; i++)
        assign Z[i] = ^A[N-1:i];
endmodule

module graycode #(
    parameter int N = 8
)(
    input  logic [N-1:0] A,
    output logic [N-1:0] G,
    output logic [N-1:0] Z
);
    binary_to_gray #(N) i_b2g (A,G);
    gray_to_binary #(N) i_g2b (G,Z);
endmodule

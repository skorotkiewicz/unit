// vm/primitives.rs — Kernel primitive word implementations
//
// These are the minimum words a Forth needs: stack manipulation,
// arithmetic, memory access, I/O, and debugging.

use crate::types::{Cell, Instruction};
use std::io::{self, Read};

impl super::VM {
    // -----------------------------------------------------------------------

    pub(crate) fn prim_dup(&mut self) {
        if let Some(&val) = self.stack.last() {
            self.stack.push(val);
        } else {
            eprintln!("stack underflow");
        }
    }

    pub(crate) fn prim_drop(&mut self) {
        self.pop();
    }

    pub(crate) fn prim_swap(&mut self) {
        let len = self.stack.len();
        if len < 2 {
            eprintln!("stack underflow");
            return;
        }
        self.stack.swap(len - 1, len - 2);
    }

    pub(crate) fn prim_over(&mut self) {
        let len = self.stack.len();
        if len < 2 {
            eprintln!("stack underflow");
            return;
        }
        self.stack.push(self.stack[len - 2]);
    }

    pub(crate) fn prim_rot(&mut self) {
        let len = self.stack.len();
        if len < 3 {
            eprintln!("stack underflow");
            return;
        }
        let val = self.stack.remove(len - 3);
        self.stack.push(val);
    }

    // -----------------------------------------------------------------------
    // Memory
    // -----------------------------------------------------------------------

    pub(crate) fn prim_fetch(&mut self) {
        let addr = self.pop() as usize;
        if addr < self.memory.len() {
            self.stack.push(self.memory[addr]);
        } else {
            eprintln!("invalid address: {}", addr);
            self.stack.push(0);
        }
    }

    pub(crate) fn prim_store(&mut self) {
        let addr = self.pop() as usize;
        let val = self.pop();
        if addr < self.memory.len() {
            self.memory[addr] = val;
        } else {
            eprintln!("invalid address: {}", addr);
        }
    }

    // -----------------------------------------------------------------------
    // Arithmetic
    // -----------------------------------------------------------------------

    pub(crate) fn prim_add(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(a.wrapping_add(b));
    }

    pub(crate) fn prim_sub(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(a.wrapping_sub(b));
    }

    pub(crate) fn prim_mul(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(a.wrapping_mul(b));
    }

    pub(crate) fn prim_div(&mut self) {
        let b = self.pop();
        let a = self.pop();
        if b == 0 {
            self.emit_str("error: division by zero\n");
            self.stack.push(0);
        } else {
            self.stack.push(a / b);
        }
    }

    pub(crate) fn prim_mod(&mut self) {
        let b = self.pop();
        let a = self.pop();
        if b == 0 {
            self.emit_str("error: division by zero\n");
            self.stack.push(0);
        } else {
            self.stack.push(a % b);
        }
    }

    // -----------------------------------------------------------------------
    // Comparison (Forth convention: -1 = true, 0 = false)
    // -----------------------------------------------------------------------

    pub(crate) fn prim_eq(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(if a == b { -1 } else { 0 });
    }

    pub(crate) fn prim_lt(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(if a < b { -1 } else { 0 });
    }

    pub(crate) fn prim_gt(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(if a > b { -1 } else { 0 });
    }

    // -----------------------------------------------------------------------
    // Logic
    // -----------------------------------------------------------------------

    pub(crate) fn prim_and(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(a & b);
    }

    pub(crate) fn prim_or(&mut self) {
        let b = self.pop();
        let a = self.pop();
        self.stack.push(a | b);
    }

    pub(crate) fn prim_not(&mut self) {
        let a = self.pop();
        self.stack.push(if a == 0 { -1 } else { 0 });
    }

    // -----------------------------------------------------------------------
    // I/O
    // -----------------------------------------------------------------------

    pub(crate) fn prim_emit(&mut self) {
        let code = self.pop();
        if let Some(ch) = char::from_u32(code as u32) {
            self.emit_char(ch);
        }
    }

    pub(crate) fn prim_key(&mut self) {
        let stdin = io::stdin();
        let mut buf = [0u8; 1];
        if stdin.lock().read_exact(&mut buf).is_ok() {
            self.stack.push(buf[0] as Cell);
        } else {
            self.stack.push(-1);
        }
    }

    pub(crate) fn prim_cr(&mut self) {
        self.emit_str("\n");
    }

    pub(crate) fn prim_dot(&mut self) {
        let val = self.pop();
        self.emit_str(&format!("{} ", val));
    }

    pub(crate) fn prim_dot_s(&mut self) {
        let s = format!("<{}> ", self.stack.len());
        self.emit_str(&s);
        let vals: Vec<String> = self.stack.iter().map(|v| format!("{} ", v)).collect();
        for v in &vals {
            self.emit_str(v);
        }
    }
    // -----------------------------------------------------------------------
    // Introspection
    // -----------------------------------------------------------------------

    pub(crate) fn prim_words(&mut self) {
        let names: Vec<String> = self
            .dictionary
            .iter()
            .rev()
            .filter(|e| !e.hidden)
            .map(|e| e.name.clone())
            .collect();
        let mut count = 0;
        for name in &names {
            self.emit_str(&format!("{} ", name));
            count += 1;
            if count % 12 == 0 {
                self.emit_str("\n");
            }
        }
        self.emit_str("\n");
    }

    pub(crate) fn prim_see(&mut self) {
        if let Some(name) = self.next_word() {
            let upper = name.to_uppercase();
            if let Some(idx) = self.find_word(&upper) {
                // Collect the entire decompilation into a string first
                // to avoid borrow conflicts with emit_str.
                let entry = &self.dictionary[idx];
                let mut out = format!(": {} ", entry.name);
                for instr in &entry.body {
                    match instr {
                        Instruction::Primitive(id) => {
                            let pname = self
                                .primitive_names
                                .iter()
                                .find(|(_, pid)| pid == id)
                                .map(|(n, _)| n.as_str())
                                .unwrap_or("?PRIM");
                            out.push_str(&format!("{} ", pname));
                        }
                        Instruction::Literal(val) => out.push_str(&format!("LIT({}) ", val)),
                        Instruction::Call(cidx) => {
                            if *cidx < self.dictionary.len() {
                                out.push_str(&format!("{} ", self.dictionary[*cidx].name));
                            } else {
                                out.push_str(&format!("CALL({}) ", cidx));
                            }
                        }
                        Instruction::StringLit(s) => out.push_str(&format!(".\" {}\" ", s)),
                        Instruction::Branch(off) => out.push_str(&format!("BRANCH({}) ", off)),
                        Instruction::BranchIfZero(off) => {
                            out.push_str(&format!("0BRANCH({}) ", off))
                        }
                    }
                }
                out.push_str(";\n");
                self.emit_str(&out);
            } else {
                self.emit_str(&format!("{}?\n", upper));
            }
        } else {
            self.emit_str("expected word name after SEE\n");
        }
    }

    // -----------------------------------------------------------------------
    // REPL control
    // -----------------------------------------------------------------------

    pub(crate) fn prim_recurse(&mut self) {
        // Immediate: compile a self-call to the word currently being defined.
        if let Some(ref mut def) = self.current_def {
            let target = self.dictionary.len(); // where this def will land
            def.body.push(Instruction::Call(target));
        }
    }

    pub(crate) fn prim_quit(&mut self) {
        self.running = false;
    }

    pub(crate) fn prim_bye(&mut self) {
        // Auto-save on graceful shutdown.
        if self.auto_save_enabled {
            if let Some(id) = self.node_id_cache {
                let snap = self.make_snapshot();
                let data = crate::persist::serialize_snapshot(&snap);
                let _ = crate::persist::save_state(&id, &data);
            }
        }
        self.running = false;
    }
}

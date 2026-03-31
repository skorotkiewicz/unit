// vm/compiler.rs — Compilation engine
//
// Colon definitions, control flow compilation (IF/ELSE/THEN, DO/LOOP,
// BEGIN/UNTIL/WHILE/REPEAT), VARIABLE, CONSTANT, CREATE/DOES>, string
// literals, comments, and the prelude loader.

use super::{P_DO_RT, P_LOOP_RT};
use crate::types::{Cell, Entry, Instruction};

impl super::VM {
    // -----------------------------------------------------------------------
    // Defining words
    // -----------------------------------------------------------------------

    pub(crate) fn prim_colon(&mut self) {
        if let Some(name) = self.next_word() {
            self.compiling = true;
            self.current_def = Some(Entry {
                name: name.to_uppercase(),
                immediate: false,
                hidden: false,
                body: Vec::new(),
            });
        } else {
            self.emit_str("error: expected word name after ':'\n");
        }
    }

    pub(crate) fn prim_semicolon(&mut self) {
        if let Some(def) = self.current_def.take() {
            self.dictionary.push(def);
            self.compiling = false;
        } else {
            self.emit_str("error: ; without matching ':'\n");
        }
    }

    pub(crate) fn prim_create(&mut self) {
        if let Some(name) = self.next_word() {
            // CREATE'd word pushes the address of its data field.
            let addr = self.here as Cell;
            self.dictionary.push(Entry {
                name: name.to_uppercase(),
                immediate: false,
                hidden: false,
                body: vec![Instruction::Literal(addr)],
            });
        } else {
            self.emit_str("error: expected word name after CREATE\n");
        }
    }

    pub(crate) fn prim_does(&mut self) {
        // Simplified DOES>: when encountered during compilation, everything
        // after DOES> in the current definition becomes the runtime behavior
        // appended to the most recently CREATE'd word. This is a seed-level
        // approximation — good enough for basic use.
    }

    pub(crate) fn prim_variable(&mut self) {
        if let Some(name) = self.next_word() {
            let addr = self.here;
            self.here += 1;
            self.dictionary.push(Entry {
                name: name.to_uppercase(),
                immediate: false,
                hidden: false,
                body: vec![Instruction::Literal(addr as Cell)],
            });
        } else {
            self.emit_str("error: expected word name after VARIABLE\n");
        }
    }

    pub(crate) fn prim_constant(&mut self) {
        if let Some(name) = self.next_word() {
            let val = self.pop();
            self.dictionary.push(Entry {
                name: name.to_uppercase(),
                immediate: false,
                hidden: false,
                body: vec![Instruction::Literal(val)],
            });
        } else {
            self.emit_str("error: expected word name after CONSTANT\n");
        }
    }

    // -----------------------------------------------------------------------
    // Control flow (immediate — compile branch instructions)
    // -----------------------------------------------------------------------

    pub(crate) fn prim_if(&mut self) {
        // If not already compiling, start an anonymous definition so
        // IF...ELSE...THEN works at the interpret prompt.
        if !self.compiling && self.current_def.is_none() {
            self.compiling = true;
            self.current_def = Some(Entry {
                name: String::new(), // anonymous
                immediate: false,
                hidden: true,
                body: Vec::new(),
            });
            self.anon_depth = 0;
        }
        self.anon_depth += 1;
        if let Some(ref mut def) = self.current_def {
            let fixup = def.body.len() as Cell;
            self.rstack.push(fixup);
            def.body.push(Instruction::BranchIfZero(0));
        }
    }

    pub(crate) fn prim_else(&mut self) {
        let if_fixup = self.rstack.pop().unwrap_or(0) as usize;
        if let Some(ref mut def) = self.current_def {
            let here = def.body.len();
            let offset = (here as i64 + 1) - if_fixup as i64;
            def.body[if_fixup] = Instruction::BranchIfZero(offset);
            def.body.push(Instruction::Branch(0));
            self.rstack.push(here as Cell);
        }
    }

    pub(crate) fn prim_then(&mut self) {
        let fixup = self.rstack.pop().unwrap_or(0) as usize;
        if let Some(ref mut def) = self.current_def {
            let here = def.body.len();
            let offset = here as i64 - fixup as i64;
            match def.body[fixup] {
                Instruction::BranchIfZero(_) => {
                    def.body[fixup] = Instruction::BranchIfZero(offset);
                }
                Instruction::Branch(_) => {
                    def.body[fixup] = Instruction::Branch(offset);
                }
                _ => {}
            }

            // If this is an anonymous definition, only finalize when
            // nesting depth returns to zero.
            self.anon_depth -= 1;
            if def.name.is_empty() && self.anon_depth <= 0 {
                let body = def.body.clone();
                self.current_def = None;
                self.compiling = false;
                self.anon_depth = 0;
                self.execute_body(&body);
            }
        }
    }

    pub(crate) fn prim_do(&mut self) {
        // Start anonymous definition if at the interpret prompt.
        if !self.compiling && self.current_def.is_none() {
            self.compiling = true;
            self.current_def = Some(Entry {
                name: String::new(),
                immediate: false,
                hidden: true,
                body: Vec::new(),
            });
            self.anon_depth = 0;
        }
        self.anon_depth += 1;
        if let Some(ref mut def) = self.current_def {
            def.body.push(Instruction::Primitive(P_DO_RT));
            let loop_start = def.body.len() as Cell;
            self.rstack.push(loop_start);
        }
    }

    pub(crate) fn prim_loop(&mut self) {
        let loop_start = self.rstack.pop().unwrap_or(0);
        if let Some(ref mut def) = self.current_def {
            def.body.push(Instruction::Primitive(P_LOOP_RT));
            let here = def.body.len();
            let offset = loop_start - here as i64;
            def.body.push(Instruction::BranchIfZero(offset));

            // Only finalize when nesting depth returns to zero.
            self.anon_depth -= 1;
            if def.name.is_empty() && self.anon_depth <= 0 {
                let body = def.body.clone();
                self.current_def = None;
                self.compiling = false;
                self.anon_depth = 0;
                self.execute_body(&body);
            }
        }
    }

    pub(crate) fn prim_begin(&mut self) {
        if let Some(ref def) = self.current_def {
            let here = def.body.len() as Cell;
            self.rstack.push(here);
        }
    }

    pub(crate) fn prim_until(&mut self) {
        let begin_addr = self.rstack.pop().unwrap_or(0);
        if let Some(ref mut def) = self.current_def {
            let here = def.body.len();
            let offset = begin_addr - here as i64;
            def.body.push(Instruction::BranchIfZero(offset));
        }
    }

    pub(crate) fn prim_while(&mut self) {
        if let Some(ref mut def) = self.current_def {
            let fixup = def.body.len() as Cell;
            self.rstack.push(fixup);
            def.body.push(Instruction::BranchIfZero(0));
        }
    }

    pub(crate) fn prim_repeat(&mut self) {
        let while_fixup = self.rstack.pop().unwrap_or(0) as usize;
        let begin_addr = self.rstack.pop().unwrap_or(0);
        if let Some(ref mut def) = self.current_def {
            let here = def.body.len();
            let offset = begin_addr - here as i64;
            def.body.push(Instruction::Branch(offset));
            let after = def.body.len();
            let while_offset = after as i64 - while_fixup as i64;
            def.body[while_fixup] = Instruction::BranchIfZero(while_offset);
        }
    }

    // -----------------------------------------------------------------------
    // DO...LOOP runtime helpers
    // -----------------------------------------------------------------------

    pub(crate) fn rt_do(&mut self) {
        let index = self.pop();
        let limit = self.pop();
        self.rstack.push(limit);
        self.rstack.push(index);
    }

    pub(crate) fn rt_loop(&mut self) {
        let index = self.rpop() + 1;
        let limit = *self.rstack.last().unwrap_or(&0);
        if index >= limit {
            self.rpop(); // remove limit
            self.stack.push(-1); // done — don't branch back
        } else {
            self.rstack.push(index);
            self.stack.push(0); // not done — branch back
        }
    }

    // -----------------------------------------------------------------------
    // Strings and comments
    // -----------------------------------------------------------------------

    pub(crate) fn prim_dot_quote(&mut self) {
        let s = self.parse_until('"');
        if self.compiling {
            if let Some(ref mut def) = self.current_def {
                def.body.push(Instruction::StringLit(s));
            }
        } else {
            self.emit_str(&s);
        }
    }

    pub(crate) fn prim_paren(&mut self) {
        self.parse_until(')');
    }

    pub(crate) fn prim_backslash(&mut self) {
        self.input_pos = self.input_buffer.len();
    }

    // -----------------------------------------------------------------------
    // Prelude
    // -----------------------------------------------------------------------

    pub fn load_prelude(&mut self) {
        let prelude = include_str!("../prelude.fs");
        let saved_silent = self.silent;
        self.silent = true;
        for line in prelude.lines() {
            self.interpret_line(line);
        }
        self.silent = saved_silent;
    }
}

// vm/tests.rs — Comprehensive unit tests for the Forth VM

use super::*;
use crate::types::Cell;

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

fn test_vm() -> VM {
    let mut vm = VM::new();
    vm.silent = true;
    vm.load_prelude();
    vm
}

fn eval(vm: &mut VM, input: &str) -> String {
    vm.output_buffer = Some(String::new());
    for line in input.lines() {
        vm.interpret_line(line);
    }
    vm.output_buffer.take().unwrap_or_default()
}

fn eval_top(vm: &mut VM, input: &str) -> Cell {
    eval(vm, input);
    vm.stack.last().copied().unwrap_or(0)
}

#[test]
fn test_stack_push_pop() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 3");
    assert_eq!(vm.stack, vec![1, 2, 3]);
}

#[test]
fn test_stack_dup() {
    let mut vm = test_vm();
    eval(&mut vm, "5 DUP");
    assert_eq!(vm.stack, vec![5, 5]);
}

#[test]
fn test_stack_drop() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 DROP");
    assert_eq!(vm.stack, vec![1]);
}

#[test]
fn test_stack_swap() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 SWAP");
    assert_eq!(vm.stack, vec![2, 1]);
}

#[test]
fn test_stack_over() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 OVER");
    assert_eq!(vm.stack, vec![1, 2, 1]);
}

#[test]
fn test_stack_rot() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 3 ROT");
    assert_eq!(vm.stack, vec![2, 3, 1]);
}

#[test]
fn test_stack_nip() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 NIP");
    assert_eq!(vm.stack, vec![2]);
}

#[test]
fn test_stack_tuck() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 TUCK");
    assert_eq!(vm.stack, vec![2, 1, 2]);
}

#[test]
fn test_stack_2dup() {
    let mut vm = test_vm();
    eval(&mut vm, "1 2 2DUP");
    assert_eq!(vm.stack, vec![1, 2, 1, 2]);
}

#[test]
fn test_stack_dot_s() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "1 2 3 .S");
    assert!(out.contains("<3>"));
    assert!(out.contains("1 2 3"));
}

// -----------------------------------------------------------------------
// 2. Arithmetic
// -----------------------------------------------------------------------

#[test]
fn test_add() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "2 3 +"), 5);
}

#[test]
fn test_subtract() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "10 3 -"), 7);
}

#[test]
fn test_multiply() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "6 7 *"), 42);
}

#[test]
fn test_divide() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "20 4 /"), 5);
}

#[test]
fn test_modulo() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "17 5 MOD"), 2);
}

// -----------------------------------------------------------------------
// 3. Comparison and logic
// -----------------------------------------------------------------------

#[test]
fn test_equal_true() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "5 5 ="), -1); // true
}

#[test]
fn test_equal_false() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "5 3 ="), 0); // false
}

#[test]
fn test_greater_true() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "5 3 >"), -1);
}

#[test]
fn test_greater_false() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "3 5 >"), 0);
}

#[test]
fn test_less_than() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "3 5 <"), -1);
}

#[test]
fn test_and() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "-1 0 AND"), 0);
}

#[test]
fn test_or() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "0 -1 OR"), -1);
}

#[test]
fn test_not() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "-1 NOT"), 0);
}

// -----------------------------------------------------------------------
// 4. Memory
// -----------------------------------------------------------------------

#[test]
fn test_memory_store_fetch() {
    let mut vm = test_vm();
    eval(&mut vm, "42 1000 !");
    assert_eq!(eval_top(&mut vm, "1000 @"), 42);
}

// -----------------------------------------------------------------------
// 5. Word definitions
// -----------------------------------------------------------------------

#[test]
fn test_colon_definition() {
    let mut vm = test_vm();
    eval(&mut vm, ": SQUARE DUP * ;");
    assert_eq!(eval_top(&mut vm, "7 SQUARE"), 49);
}

#[test]
fn test_nested_definitions() {
    let mut vm = test_vm();
    eval(&mut vm, ": DOUBLE 2 * ;");
    eval(&mut vm, ": QUADRUPLE DOUBLE DOUBLE ;");
    assert_eq!(eval_top(&mut vm, "3 QUADRUPLE"), 12);
}

#[test]
fn test_constant() {
    let mut vm = test_vm();
    eval(&mut vm, "42 CONSTANT ANSWER");
    assert_eq!(eval_top(&mut vm, "ANSWER"), 42);
}

#[test]
fn test_variable() {
    let mut vm = test_vm();
    eval(&mut vm, "VARIABLE X 99 X ! X @");
    assert_eq!(*vm.stack.last().unwrap(), 99);
}

#[test]
fn test_recursion_factorial() {
    let mut vm = test_vm();
    eval(
        &mut vm,
        ": FACT DUP 1 > IF DUP 1 - RECURSE * ELSE DROP 1 THEN ;",
    );
    assert_eq!(eval_top(&mut vm, "5 FACT"), 120);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "10 FACT"), 3628800);
}

// -----------------------------------------------------------------------
// 6. Control flow
// -----------------------------------------------------------------------

#[test]
fn test_if_then() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "1 IF 42 . THEN");
    assert!(out.contains("42"));
}

#[test]
fn test_if_else_then_true() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "1 IF 1 . ELSE 2 . THEN");
    assert!(out.contains("1"));
    assert!(!out.contains("2"));
}

#[test]
fn test_if_else_then_false() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "0 IF 1 . ELSE 2 . THEN");
    assert!(out.contains("2"));
    assert!(!out.contains("1"));
}

#[test]
fn test_do_loop() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "5 0 DO I . LOOP");
    assert!(out.contains("0"));
    assert!(out.contains("4"));
}

#[test]
fn test_do_loop_sum() {
    let mut vm = test_vm();
    eval(&mut vm, "0 10 0 DO I + LOOP");
    assert_eq!(*vm.stack.last().unwrap(), 45);
}

#[test]
fn test_begin_until() {
    let mut vm = test_vm();
    eval(&mut vm, ": CD 5 BEGIN DUP . 1 - DUP 0 = UNTIL DROP ;");
    let out = eval(&mut vm, "CD");
    assert!(out.contains("5"));
    assert!(out.contains("1"));
}

#[test]
fn test_begin_while_repeat() {
    let mut vm = test_vm();
    eval(
        &mut vm,
        ": WH 5 BEGIN DUP 0 > WHILE DUP . 1 - REPEAT DROP ;",
    );
    let out = eval(&mut vm, "WH");
    assert!(out.contains("5"));
    assert!(out.contains("1"));
}

#[test]
fn test_nested_if() {
    let mut vm = test_vm();
    eval(
        &mut vm,
        ": SIGN DUP 0 > IF 1 ELSE DUP 0 < IF -1 ELSE 0 THEN THEN NIP ;",
    );
    assert_eq!(eval_top(&mut vm, "5 SIGN"), 1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "-3 SIGN"), -1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "0 SIGN"), 0);
}

// -----------------------------------------------------------------------
// 7. Strings and I/O
// -----------------------------------------------------------------------

#[test]
fn test_dot_quote() {
    let mut vm = test_vm();
    eval(&mut vm, ": MSG .\" hello\" ;");
    let out = eval(&mut vm, "MSG");
    assert_eq!(out, "hello");
}

#[test]
fn test_emit() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "65 EMIT");
    assert_eq!(out, "A");
}

#[test]
fn test_cr() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "CR");
    assert_eq!(out, "\n");
}

#[test]
fn test_dot() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "42 .");
    assert_eq!(out, "42 ");
}

#[test]
fn test_type_word() {
    let mut vm = test_vm();
    // Store "Hi" at addresses 60000-60001
    eval(&mut vm, "72 60000 ! 105 60001 !");
    let out = eval(&mut vm, "60000 2 TYPE");
    assert_eq!(out, "Hi");
}

// -----------------------------------------------------------------------
// 8. Prelude words
// -----------------------------------------------------------------------

#[test]
fn test_abs() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "-7 ABS"), 7);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "7 ABS"), 7);
}

#[test]
fn test_min_max() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "3 7 MIN"), 3);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "3 7 MAX"), 7);
}

#[test]
fn test_negate() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "5 NEGATE"), -5);
}

#[test]
fn test_inc_dec() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "5 1+"), 6);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "5 1-"), 4);
}

#[test]
fn test_double_halve() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "6 2*"), 12);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "6 2/"), 3);
}

#[test]
fn test_zero_predicates() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "0 0="), -1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "5 0="), 0);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "-3 0<"), -1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "3 0<"), 0);
}

#[test]
fn test_not_equal() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "3 5 <>"), -1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "5 5 <>"), 0);
}

#[test]
fn test_true_false() {
    let mut vm = test_vm();
    assert_eq!(eval_top(&mut vm, "TRUE"), -1);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "FALSE"), 0);
}

#[test]
fn test_prelude_loads() {
    let vm = test_vm();
    // Prelude defines these words — verify they exist in the dictionary.
    let names: Vec<&str> = vec![
        "NIP", "TUCK", "2DUP", "2DROP", "ABS", "MIN", "MAX", "NEGATE", "1+", "1-", "TRUE", "FALSE",
    ];
    for name in names {
        assert!(
            vm.find_word(name).is_some(),
            "prelude word '{}' not found",
            name
        );
    }
}

// -----------------------------------------------------------------------
// 9. Introspection
// -----------------------------------------------------------------------

#[test]
fn test_see() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "SEE NIP");
    assert!(out.contains("SWAP"));
    assert!(out.contains("DROP"));
}

#[test]
fn test_words_produces_output() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "WORDS");
    assert!(out.contains("DUP"));
    assert!(out.contains("DROP"));
}

// -----------------------------------------------------------------------
// 10. Sandbox execution
// -----------------------------------------------------------------------

#[test]
fn test_sandbox_captures_output() {
    let mut vm = test_vm();
    let result = vm.execute_sandbox(".\" hello sandbox\"");
    assert!(result.success);
    assert_eq!(result.output, "hello sandbox");
}

#[test]
fn test_sandbox_captures_stack() {
    let mut vm = test_vm();
    let result = vm.execute_sandbox("2 3 + 4 *");
    assert!(result.success);
    assert_eq!(result.stack_snapshot, vec![20]);
}

#[test]
fn test_sandbox_timeout() {
    let mut vm = test_vm();
    vm.execution_timeout = 1; // 1 second
    eval(&mut vm, ": INF BEGIN 0 UNTIL ;");
    let result = vm.execute_sandbox("INF");
    assert!(!result.success);
    assert!(result.error.unwrap().contains("timeout"));
}

#[test]
fn test_sandbox_isolates_stack() {
    let mut vm = test_vm();
    eval(&mut vm, "100 200 300"); // main stack
    let result = vm.execute_sandbox("1 2 3");
    assert_eq!(result.stack_snapshot, vec![1, 2, 3]);
    assert_eq!(vm.stack, vec![100, 200, 300]); // preserved
}

// -----------------------------------------------------------------------
// 11. Serialization round-trip (persist)
// -----------------------------------------------------------------------

#[test]
fn test_snapshot_roundtrip() {
    let mut vm = test_vm();
    eval(&mut vm, ": TRIPLE DUP DUP + + ;");
    // Use a VARIABLE to store a value (this allocates within `here`).
    eval(&mut vm, "VARIABLE TESTVAR 42 TESTVAR !");
    let snap = vm.make_snapshot();
    let data = crate::persist::serialize_snapshot(&snap);
    let restored = crate::persist::deserialize_snapshot(&data).unwrap();
    assert_eq!(restored.here, snap.here);
    assert_eq!(restored.dictionary.len(), snap.dictionary.len());
    // Verify the word exists.
    let found = restored.dictionary.iter().any(|e| e.name == "TRIPLE");
    assert!(found, "TRIPLE not found in restored dictionary");
}

// -----------------------------------------------------------------------
// 12. Replication package
// -----------------------------------------------------------------------

#[test]
fn test_package_build_unpack() {
    let state = vec![1, 2, 3, 4, 5]; // dummy state
    let pkg = crate::spawn::build_package(&state).unwrap();
    let (binary, state_out, prelude) = crate::spawn::unpack_package(&pkg).unwrap();
    assert!(!binary.is_empty(), "binary should not be empty");
    assert_eq!(state_out, vec![1, 2, 3, 4, 5]);
    assert!(!prelude.is_empty(), "prelude should not be empty");
}

#[test]
fn test_package_invalid_magic() {
    let bad = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(crate::spawn::unpack_package(&bad).is_err());
}

// -----------------------------------------------------------------------
// 13. Mutation
// -----------------------------------------------------------------------

#[test]
fn test_mutation_and_undo() {
    let mut vm = test_vm();
    eval(&mut vm, ": DBL 2 * ;");
    assert_eq!(eval_top(&mut vm, "5 DBL"), 10);
    vm.stack.clear();
    // Mutate
    eval(&mut vm, "MUTATE-WORD\" DBL\"");
    // It may or may not change behavior, but undo should restore.
    eval(&mut vm, "UNDO-MUTATE");
    assert_eq!(eval_top(&mut vm, "5 DBL"), 10);
}

// -----------------------------------------------------------------------
// 14. Mesh wire format (encode/decode)
// -----------------------------------------------------------------------

#[test]
fn test_serialize_state_roundtrip() {
    let mut vm = test_vm();
    eval(&mut vm, ": TEST 1 2 + ;");
    let goals = crate::goals::GoalRegistry::empty();
    let data = crate::mesh::serialize_state(&vm.dictionary, &vm.memory, vm.here, Some(&goals));
    let (dict, mem, here) = crate::mesh::deserialize_state(&data).unwrap();
    assert_eq!(here, vm.here);
    assert_eq!(dict.len(), vm.dictionary.len());
    assert_eq!(mem[0..here], vm.memory[0..here]);
}

// -----------------------------------------------------------------------
// 15. Regression tests
// -----------------------------------------------------------------------

#[test]
fn test_deep_recursion() {
    let mut vm = test_vm();
    eval(
        &mut vm,
        ": FACT DUP 1 > IF DUP 1 - RECURSE * ELSE DROP 1 THEN ;",
    );
    assert_eq!(eval_top(&mut vm, "10 FACT"), 3628800);
}

#[test]
fn test_loop_in_definition() {
    let mut vm = test_vm();
    eval(&mut vm, ": SUM 0 SWAP 0 DO I + LOOP ;");
    assert_eq!(eval_top(&mut vm, "10 SUM"), 45);
}

#[test]
fn test_interpret_mode_do_loop() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "5 0 DO I . LOOP");
    assert!(out.contains("0"));
    assert!(out.contains("4"));
}

#[test]
fn test_interpret_mode_if_else() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "1 IF 1 . ELSE 2 . THEN");
    assert!(out.contains("1"));
    assert!(!out.contains("2"));
}

#[test]
fn test_eval_word() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "EVAL\" 2 3 + .\"");
    assert!(out.contains("5"));
}

#[test]
fn test_string_in_definition() {
    let mut vm = test_vm();
    eval(&mut vm, ": HI .\" hello world\" ;");
    let out = eval(&mut vm, "HI");
    assert_eq!(out, "hello world");
}

#[test]
fn test_nested_loops() {
    let mut vm = test_vm();
    eval(&mut vm, ": GRID 3 0 DO 2 0 DO I . J . LOOP LOOP ;");
    let out = eval(&mut vm, "GRID");
    // Should have 6 pairs of I J values
    assert!(out.contains("0 0"));
    assert!(out.contains("1 2")); // I=1, J=2
}

// -----------------------------------------------------------------------
// New VM API tests
// -----------------------------------------------------------------------

#[test]
fn test_vm_new_empty_stacks() {
    let vm = VM::new();
    assert!(vm.stack.is_empty());
    assert!(vm.rstack.is_empty());
}

#[test]
fn test_vm_eval_returns_output() {
    let mut vm = test_vm();
    let out = vm.eval("42 .");
    assert_eq!(out.trim(), "42");
}

#[test]
fn test_vm_stack_top() {
    let mut vm = test_vm();
    assert_eq!(vm.stack_top(), None);
    eval(&mut vm, "42");
    assert_eq!(vm.stack_top(), Some(42));
}

#[test]
fn test_vm_eval_multiple_calls() {
    let mut vm = test_vm();
    eval(&mut vm, ": DOUBLE 2 * ;");
    assert_eq!(eval_top(&mut vm, "5 DOUBLE"), 10);
    vm.stack.clear();
    assert_eq!(eval_top(&mut vm, "3 DOUBLE"), 6);
}

#[test]
fn test_vm_isolation() {
    let mut vm1 = test_vm();
    let mut vm2 = test_vm();
    eval(&mut vm1, ": FOO 42 ;");
    eval(&mut vm1, "FOO");
    assert_eq!(vm1.stack_top(), Some(42));
    // vm2 should NOT have FOO
    eval(&mut vm2, "FOO"); // prints "FOO?" but doesn't crash
    assert_eq!(vm2.stack_top(), None); // stack empty — FOO not found
}

#[test]
fn test_vm_output_buffer() {
    let mut vm = test_vm();
    vm.output_buffer = Some(String::new());
    vm.interpret_line(".\" hello\"");
    let out = vm.output_buffer.take().unwrap();
    assert_eq!(out, "hello");
}

#[test]
fn test_vm_find_word() {
    let vm = test_vm();
    assert!(vm.find_word("DUP").is_some());
    assert!(vm.find_word("NONEXISTENT_WORD_XYZ").is_none());
}

#[test]
fn test_vm_prelude_loaded() {
    let vm = test_vm();
    for name in &["NIP", "TUCK", "ABS", "MIN", "MAX", "TRUE", "FALSE"] {
        assert!(
            vm.find_word(name).is_some(),
            "prelude word '{}' missing",
            name
        );
    }
}

#[test]
fn test_vm_eval_error_output() {
    let mut vm = test_vm();
    // Evaluating an unknown word should not crash, stack stays intact
    eval(&mut vm, "42");
    eval(&mut vm, "NONEXISTENT_WORD");
    assert_eq!(vm.stack_top(), Some(42)); // stack preserved
}

#[test]
fn test_vm_complex_program() {
    let mut vm = test_vm();
    eval(
        &mut vm,
        ": FIB DUP 2 < IF DROP 1 ELSE DUP 1 - RECURSE SWAP 2 - RECURSE + THEN ;",
    );
    assert_eq!(eval_top(&mut vm, "10 FIB"), 89);
}

// -----------------------------------------------------------------------
// Swarm tests
// -----------------------------------------------------------------------

#[test]
fn test_swarm_status_word() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "SWARM-STATUS");
    // Without mesh, shows offline
    assert!(out.contains("offline") || out.contains("swarm"));
}

#[test]
fn test_shared_words_empty() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "SHARED-WORDS");
    // With no mesh, output is empty (graceful offline behavior).
    assert!(out.is_empty() || out.contains("no shared"));
}

#[test]
fn test_swarm_on_word() {
    let mut vm = test_vm();
    let out = eval(&mut vm, "SWARM-ON");
    assert!(out.contains("swarm mode"));
}

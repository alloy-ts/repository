#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use fusevm::{VM, VMResult, Value};

    use crate::engine::compiler::{Compiler, EXT_HOST_CALL_BASE};
    use crate::engine::parser::{tokenize, Parser};

    fn compile_and_run(source: &str) -> (Value, String) {
        let tokens = tokenize(source).expect("tokenize");
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().expect("parse");
        let compiler = Compiler::new(HashSet::new(), Vec::new());
        let chunk = compiler.compile(&stmts).expect("compile");

        let mut vm = VM::new(chunk);
        let stdout = Arc::new(Mutex::new(String::new()));
        let out_clone = Arc::clone(&stdout);
        vm.set_output_sink(Box::new(move |s| {
            out_clone.lock().unwrap().push_str(s);
        }));

        let res = match vm.run() {
            VMResult::Ok(val) => val,
            VMResult::Error(e) => panic!("VM error: {e}"),
            VMResult::Halted => Value::Undef,
        };
        let out = stdout.lock().unwrap().clone();
        (res, out)
    }

    #[test]
    fn test_arithmetic_precedence() {
        let (val, _) = compile_and_run("10 + 20 * 2;");
        assert_eq!(val.to_int(), 50);
    }

    #[test]
    fn test_variables_and_print() {
        let source = "let x = 10; let y = 20; let z = x + y; print(z); z;";
        let (val, stdout) = compile_and_run(source);
        assert_eq!(val.to_int(), 30);
        assert_eq!(stdout, "30\n");
    }

    #[test]
    fn test_while_loop_1_to_100() {
        let source = r#"
        let sum = 0;
        let i = 1;
        while (i <= 100) {
            sum = sum + i;
            i = i + 1;
        }
        sum;
        "#;
        let (val, _) = compile_and_run(source);
        assert_eq!(val.to_int(), 5050);
    }

    #[test]
    fn test_functions_and_recursion() {
        let source = r#"
        fn fact(n) {
            if (n <= 1) {
                return 1;
            } else {
                return n * fact(n - 1);
            }
        }
        fact(5);
        "#;
        let (val, _) = compile_and_run(source);
        assert_eq!(val.to_int(), 120);
    }

    #[test]
    fn test_host_function_lowering() {
        let source = "price(10) * 2;";
        let tokens = tokenize(source).expect("tokenize");
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().expect("parse");
        let mut host_fns = HashSet::new();
        host_fns.insert("price".to_string());
        let compiler = Compiler::new(host_fns, Vec::new());
        let chunk = compiler.compile(&stmts).expect("compile");

        let mut vm = VM::new(chunk);
        vm.set_extension_handler(Box::new(|vm, id, argc| {
            if id >= EXT_HOST_CALL_BASE {
                assert_eq!(argc, 1);
                let arg = vm.pop();
                vm.push(Value::Int(arg.to_int() + 5));
            }
        }));

        match vm.run() {
            VMResult::Ok(val) => assert_eq!(val.to_int(), 30),
            VMResult::Error(e) => panic!("VM error: {e}"),
            VMResult::Halted => panic!("Halted"),
        }
    }
}

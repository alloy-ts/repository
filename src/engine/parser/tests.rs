#[cfg(test)]
mod tests {
    use crate::engine::parser::{tokenize, Parser};
    use crate::engine::ast::{StmtKind, ExprKind, BinaryOp};

    #[test]
    fn test_lexer_and_parser_basic() {
        let source = "let x = 10 + 20 * 2;\nprint(x);";
        let tokens = tokenize(source).expect("tokenize");
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().expect("parse");
        assert_eq!(stmts.len(), 2);

        match &stmts[0].kind {
            StmtKind::Let { name, value } => {
                assert_eq!(name, "x");
                match &value.kind {
                    ExprKind::Binary { operator, right, .. } => {
                        assert_eq!(*operator, BinaryOp::Add);
                        match &right.kind {
                            ExprKind::Binary { operator, .. } => {
                                assert_eq!(*operator, BinaryOp::Multiply);
                            }
                            _ => panic!("Expected multiplication"),
                        }
                    }
                    _ => panic!("Expected binary add"),
                }
            }
            _ => panic!("Expected let stmt"),
        }
    }

    #[test]
    fn test_functions_and_loops() {
        let source = r#"
        fn fact(n) {
            if (n <= 1) {
                return 1;
            } else {
                return n * fact(n - 1);
            }
        }
        let i = 0;
        while (i < 5) {
            i = i + 1;
        }
        "#;
        let tokens = tokenize(source).expect("tokenize");
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().expect("parse");
        assert_eq!(stmts.len(), 3);
    }
}

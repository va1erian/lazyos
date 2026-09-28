//! Recursive-descent parser: tokens to a small AST.
//!
//! Precedence (low to high): `||`, `&&`, comparisons, `+ -`, `* / %`.

use super::lexer::Tok;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Var(String),
    Array(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(String, Expr),
    Print(Expr),
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
}

/// Parse a whole program.
pub fn parse(tokens: Vec<Tok>) -> Result<Vec<Stmt>, String> {
    let mut parser = Parser { tokens, pos: 0 };
    let mut stmts = Vec::new();
    parser.skip_separators();
    while !parser.eof() {
        stmts.push(parser.stmt()?);
        parser.skip_separators();
    }
    Ok(stmts)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn skip_separators(&mut self) {
        while matches!(self.peek(), Some(Tok::Semi)) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, token: &Tok) -> bool {
        if self.peek() == Some(token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: Tok) -> Result<(), String> {
        if self.eat(&token) {
            Ok(())
        } else {
            let found = self
                .peek()
                .map(Tok::describe)
                .unwrap_or_else(|| String::from("end of input"));
            Err(format!("expected {token:?}, found {found}"))
        }
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            Some(Tok::Ident(name)) if name == "let" => {
                self.pos += 1;
                self.let_stmt()
            }
            Some(Tok::Ident(name)) if name == "print" => {
                self.pos += 1;
                Ok(Stmt::Print(self.expr()?))
            }
            Some(Tok::Ident(name)) if name == "if" => {
                self.pos += 1;
                self.if_stmt()
            }
            Some(Tok::LBrace) => {
                self.pos += 1;
                Ok(Stmt::Block(self.block()?))
            }
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn let_stmt(&mut self) -> Result<Stmt, String> {
        let name = match self.next() {
            Some(Tok::Ident(name)) => name,
            _ => return Err(String::from("expected a variable name after 'let'")),
        };
        // Accept both `let x = ...` and Dyon's `let x := ...`.
        self.eat(&Tok::Colon);
        self.expect(Tok::Assign)?;
        Ok(Stmt::Let(name, self.expr()?))
    }

    fn if_stmt(&mut self) -> Result<Stmt, String> {
        let condition = self.expr()?;
        self.expect(Tok::LBrace)?;
        let then_branch = Box::new(Stmt::Block(self.block()?));
        let else_branch = if matches!(self.peek(), Some(Tok::Ident(n)) if n == "else") {
            self.pos += 1;
            if self.eat(&Tok::LBrace) {
                Some(Box::new(Stmt::Block(self.block()?)))
            } else {
                Some(Box::new(self.stmt()?))
            }
        } else {
            None
        };
        Ok(Stmt::If(condition, then_branch, else_branch))
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        self.skip_separators();
        while !matches!(self.peek(), Some(Tok::RBrace)) && !self.eof() {
            stmts.push(self.stmt()?);
            self.skip_separators();
        }
        self.expect(Tok::RBrace)?;
        Ok(stmts)
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.binary(0)
    }

    fn binary(&mut self, min_precedence: u8) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while let Some((op, precedence)) = self.peek_binop() {
            if precedence < min_precedence {
                break;
            }
            self.pos += 1;
            let right = self.binary(precedence + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn peek_binop(&self) -> Option<(BinOp, u8)> {
        Some(match self.peek()? {
            Tok::OrOr => (BinOp::Or, 1),
            Tok::AndAnd => (BinOp::And, 2),
            Tok::EqEq => (BinOp::Eq, 3),
            Tok::Ne => (BinOp::Ne, 3),
            Tok::Lt => (BinOp::Lt, 4),
            Tok::Le => (BinOp::Le, 4),
            Tok::Gt => (BinOp::Gt, 4),
            Tok::Ge => (BinOp::Ge, 4),
            Tok::Plus => (BinOp::Add, 5),
            Tok::Minus => (BinOp::Sub, 5),
            Tok::Star => (BinOp::Mul, 6),
            Tok::Slash => (BinOp::Div, 6),
            Tok::Percent => (BinOp::Rem, 6),
            _ => return None,
        })
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some(Tok::Bang) => {
                self.pos += 1;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.primary()?;
        while self.eat(&Tok::LBracket) {
            let index = self.expr()?;
            self.expect(Tok::RBracket)?;
            expr = Expr::Index(Box::new(expr), Box::new(index));
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::Ident(name)) if name == "true" => Ok(Expr::Bool(true)),
            Some(Tok::Ident(name)) if name == "false" => Ok(Expr::Bool(false)),
            Some(Tok::Ident(name)) => Ok(Expr::Var(name)),
            Some(Tok::LParen) => {
                let expr = self.expr()?;
                self.expect(Tok::RParen)?;
                Ok(expr)
            }
            Some(Tok::LBracket) => {
                let mut items = Vec::new();
                if !matches!(self.peek(), Some(Tok::RBracket)) {
                    loop {
                        items.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                self.expect(Tok::RBracket)?;
                Ok(Expr::Array(items))
            }
            other => Err(format!(
                "unexpected {}",
                other
                    .map(|t| t.describe())
                    .unwrap_or_else(|| String::from("end of input"))
            )),
        }
    }
}

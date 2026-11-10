//! Shell AST definitions for Bun-compatible shell.

#[derive(Debug, Clone, PartialEq)]
pub enum RedirectOp {
    /// `< filename`
    Input,
    /// `> filename` or `1> filename`
    OutputTruncate,
    /// `>> filename` or `1>> filename`
    OutputAppend,
    /// `2> filename`
    StderrTruncate,
    /// `2>> filename`
    StderrAppend,
    /// `&> filename`
    BothTruncate,
    /// `&>> filename`
    BothAppend,
    /// `2>&1`
    StderrToStdout,
    /// `1>&2`
    StdoutToStderr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redirection {
    pub op: RedirectOp,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandNode {
    pub env_vars: Vec<(String, String)>,
    pub args: Vec<String>,
    pub redirects: Vec<Redirection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipelineNode {
    pub commands: Vec<CommandNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogicalOp {
    And,
    Or,
    Sequential,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipelineSequence {
    pub pipelines: Vec<(LogicalOp, PipelineNode)>,
}

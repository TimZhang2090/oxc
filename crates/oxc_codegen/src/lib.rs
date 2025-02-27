//! Oxc Codegen
//!
//! Code adapted from
//! * [esbuild](https://github.com/evanw/esbuild/blob/main/internal/js_printer/js_printer.go)

mod binary_expr_visitor;
mod comment;
mod context;
mod gen;
mod operator;
mod sourcemap_builder;

use std::{borrow::Cow, path::PathBuf};

use oxc_ast::ast::{
    BindingIdentifier, BlockStatement, Expression, IdentifierReference, Program, Statement,
};
use oxc_mangler::Mangler;
use oxc_span::{GetSpan, Span};
use oxc_syntax::{
    identifier::is_identifier_part,
    operator::{BinaryOperator, UnaryOperator, UpdateOperator},
    precedence::Precedence,
};

use crate::{
    binary_expr_visitor::BinaryExpressionVisitor, comment::CommentsMap, operator::Operator,
    sourcemap_builder::SourcemapBuilder,
};
pub use crate::{
    context::Context,
    gen::{Gen, GenExpr},
};

/// Code generator without whitespace removal.
pub type CodeGenerator<'a> = Codegen<'a>;

#[derive(Debug, Clone)]
pub struct CodegenOptions {
    /// Use single quotes instead of double quotes.
    ///
    /// Default is `false`.
    pub single_quote: bool,

    /// Remove whitespace.
    ///
    /// Default is `false`.
    pub minify: bool,

    /// Print comments?
    ///
    /// Default is `true`.
    pub comments: bool,

    /// Print annotation comments, e.g. `/* #__PURE__ */` and `/* #__NO_SIDE_EFFECTS__ */`.
    ///
    /// Only takes into effect when `comments` is false.
    ///
    /// Default is `false`.
    pub annotation_comments: bool,

    pub source_map_path: Option<PathBuf>,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            single_quote: false,
            minify: false,
            comments: true,
            annotation_comments: false,
            source_map_path: None,
        }
    }
}

impl CodegenOptions {
    fn print_annotation_comments(&self) -> bool {
        !self.minify && (self.comments || self.annotation_comments)
    }
}

/// Output from [`Codegen::build`]
pub struct CodegenReturn {
    /// The generated source code.
    pub code: String,

    /// The source map from the input source code to the generated source code.
    ///
    /// You must set [`CodegenOptions::source_map_path`] for this to be [`Some`].
    pub map: Option<oxc_sourcemap::SourceMap>,
}

pub struct Codegen<'a> {
    pub(crate) options: CodegenOptions,

    /// Original source code of the AST
    source_text: &'a str,

    comments: CommentsMap,

    mangler: Option<Mangler>,

    /// Output Code
    code: Vec<u8>,

    // states
    prev_op_end: usize,
    prev_reg_exp_end: usize,
    need_space_before_dot: usize,
    print_next_indent_as_space: bool,
    binary_expr_stack: Vec<BinaryExpressionVisitor<'a>>,

    /// For avoiding `;` if the previous statement ends with `}`.
    needs_semicolon: bool,

    prev_op: Option<Operator>,

    start_of_stmt: usize,
    start_of_arrow_expr: usize,
    start_of_default_export: usize,
    /// Start of comment that needs to be moved to the before VariableDeclarator
    ///
    /// For example:
    /// ```js
    ///  /* @__NO_SIDE_EFFECTS__ */ export const a = function() {
    ///  }, b = 10000;
    /// ```
    /// Should be generated as:
    /// ```js
    ///   export const /* @__NO_SIDE_EFFECTS__ */ a = function() {
    ///  }, b = 10000;
    /// ```
    start_of_annotation_comment: Option<u32>,

    /// Track the current indentation level
    indent: u32,

    /// Fast path for [CodegenOptions::single_quote]
    quote: u8,

    // Builders
    sourcemap_builder: Option<SourcemapBuilder>,
}

impl<'a> Default for Codegen<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> From<Codegen<'a>> for String {
    fn from(mut val: Codegen<'a>) -> Self {
        val.into_source_text()
    }
}

impl<'a> From<Codegen<'a>> for Cow<'a, str> {
    fn from(mut val: Codegen<'a>) -> Self {
        Cow::Owned(val.into_source_text())
    }
}

// Public APIs
impl<'a> Codegen<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            options: CodegenOptions::default(),
            source_text: "",
            comments: CommentsMap::default(),
            start_of_annotation_comment: None,
            mangler: None,
            code: vec![],
            needs_semicolon: false,
            need_space_before_dot: 0,
            print_next_indent_as_space: false,
            binary_expr_stack: Vec::with_capacity(5),
            prev_op_end: 0,
            prev_reg_exp_end: 0,
            prev_op: None,
            start_of_stmt: 0,
            start_of_arrow_expr: 0,
            start_of_default_export: 0,
            indent: 0,
            quote: b'"',
            sourcemap_builder: None,
        }
    }

    #[must_use]
    pub fn with_options(mut self, options: CodegenOptions) -> Self {
        self.quote = if options.single_quote { b'\'' } else { b'"' };
        self.options = options;
        self
    }

    #[must_use]
    pub fn with_mangler(mut self, mangler: Option<Mangler>) -> Self {
        self.mangler = mangler;
        self
    }

    #[must_use]
    pub fn build(mut self, program: &Program<'a>) -> CodegenReturn {
        self.quote = if self.options.single_quote { b'\'' } else { b'"' };
        self.source_text = program.source_text;
        self.code.reserve(program.source_text.len());
        if self.options.print_annotation_comments() {
            self.build_comments(&program.comments);
        }
        if let Some(path) = &self.options.source_map_path {
            self.sourcemap_builder = Some(SourcemapBuilder::new(path, program.source_text));
        }

        program.print(&mut self, Context::default());
        let code = self.into_source_text();
        let map = self.sourcemap_builder.map(SourcemapBuilder::into_sourcemap);
        CodegenReturn { code, map }
    }

    #[must_use]
    pub fn into_source_text(&mut self) -> String {
        // SAFETY: criteria of `from_utf8_unchecked` are met.
        unsafe { String::from_utf8_unchecked(std::mem::take(&mut self.code)) }
    }

    /// Push a single character into the buffer
    #[inline]
    pub fn print_char(&mut self, ch: u8) {
        self.code.push(ch);
    }

    /// Push str into the buffer
    #[inline]
    pub fn print_str(&mut self, s: &str) {
        self.code.extend(s.as_bytes());
    }

    #[inline]
    pub fn print_expression(&mut self, expr: &Expression<'_>) {
        expr.print_expr(self, Precedence::Lowest, Context::empty());
    }
}

// Private APIs
impl<'a> Codegen<'a> {
    fn code(&self) -> &Vec<u8> {
        &self.code
    }

    fn code_len(&self) -> usize {
        self.code().len()
    }

    #[inline]
    fn print_soft_space(&mut self) {
        if !self.options.minify {
            self.print_char(b' ');
        }
    }

    #[inline]
    fn print_hard_space(&mut self) {
        self.print_char(b' ');
    }

    #[inline]
    fn print_soft_newline(&mut self) {
        if !self.options.minify {
            self.print_char(b'\n');
        }
    }

    #[inline]
    fn print_hard_newline(&mut self) {
        self.print_char(b'\n');
    }

    #[inline]
    fn print_semicolon(&mut self) {
        self.print_char(b';');
    }

    #[inline]
    fn print_comma(&mut self) {
        self.print_char(b',');
    }

    #[inline]
    fn print_space_before_identifier(&mut self) {
        if self
            .peek_nth(0)
            .is_some_and(|ch| is_identifier_part(ch) || self.prev_reg_exp_end == self.code.len())
        {
            self.print_hard_space();
        }
    }

    #[inline]
    fn peek_nth(&self, n: usize) -> Option<char> {
        // SAFETY: criteria of `from_utf8_unchecked` are met.
        unsafe { std::str::from_utf8_unchecked(self.code()) }.chars().nth_back(n)
    }

    #[inline]
    fn indent(&mut self) {
        if !self.options.minify {
            self.indent += 1;
        }
    }

    #[inline]
    fn dedent(&mut self) {
        if !self.options.minify {
            self.indent -= 1;
        }
    }

    #[inline]
    fn print_indent(&mut self) {
        if self.options.minify {
            return;
        }
        if self.print_next_indent_as_space {
            self.print_hard_space();
            self.print_next_indent_as_space = false;
            return;
        }
        self.code.extend(std::iter::repeat(b'\t').take(self.indent as usize));
    }

    #[inline]
    fn print_semicolon_after_statement(&mut self) {
        if self.options.minify {
            self.needs_semicolon = true;
        } else {
            self.print_str(";\n");
        }
    }

    #[inline]
    fn print_semicolon_if_needed(&mut self) {
        if self.needs_semicolon {
            self.print_semicolon();
            self.needs_semicolon = false;
        }
    }

    #[inline]
    fn print_ellipsis(&mut self) {
        self.print_str("...");
    }

    #[inline]
    fn print_colon(&mut self) {
        self.print_char(b':');
    }

    #[inline]
    fn print_equal(&mut self) {
        self.print_char(b'=');
    }

    fn print_sequence<T: Gen>(&mut self, items: &[T], ctx: Context) {
        for item in items {
            item.print(self, ctx);
            self.print_comma();
        }
    }

    fn print_curly_braces<F: FnOnce(&mut Self)>(&mut self, span: Span, single_line: bool, op: F) {
        self.add_source_mapping(span.start);
        self.print_char(b'{');
        if !single_line {
            self.print_soft_newline();
            self.indent();
        }
        op(self);
        if !single_line {
            self.dedent();
            self.print_indent();
        }
        self.add_source_mapping(span.end);
        self.print_char(b'}');
    }

    fn print_block_start(&mut self, position: u32) {
        self.add_source_mapping(position);
        self.print_char(b'{');
        self.print_soft_newline();
        self.indent();
    }

    fn print_block_end(&mut self, position: u32) {
        self.dedent();
        self.print_indent();
        self.add_source_mapping(position);
        self.print_char(b'}');
    }

    fn print_body(&mut self, stmt: &Statement<'_>, need_space: bool, ctx: Context) {
        match stmt {
            Statement::BlockStatement(stmt) => {
                self.print_soft_space();
                self.print_block_statement(stmt, ctx);
                self.print_soft_newline();
            }
            Statement::EmptyStatement(_) => {
                self.print_semicolon();
                self.print_soft_newline();
            }
            stmt => {
                if need_space && self.options.minify {
                    self.print_hard_space();
                }
                self.print_next_indent_as_space = true;
                stmt.print(self, ctx);
            }
        }
    }

    fn print_block_statement(&mut self, stmt: &BlockStatement<'_>, ctx: Context) {
        self.print_curly_braces(stmt.span, stmt.body.is_empty(), |p| {
            for stmt in &stmt.body {
                p.print_semicolon_if_needed();
                stmt.print(p, ctx);
            }
        });
        self.needs_semicolon = false;
    }

    // We tried optimizing this to move the `index != 0` check out of the loop:
    // ```
    // let mut iter = items.iter();
    // let Some(item) = iter.next() else { return };
    // item.print(self, ctx);
    // for item in iter {
    //     self.print_comma();
    //     self.print_soft_space();
    //     item.print(self, ctx);
    // }
    // ```
    // But it turned out this was actually a bit slower.
    // <https://github.com/oxc-project/oxc/pull/5221>
    fn print_list<T: Gen>(&mut self, items: &[T], ctx: Context) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
                self.print_soft_space();
            }
            item.print(self, ctx);
        }
    }

    fn print_list_with_comments<T: Gen + GetSpan>(&mut self, items: &[T], ctx: Context) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
            }
            if self.has_non_annotation_comment(item.span().start) {
                self.print_expr_comments(item.span().start);
                self.print_indent();
            } else {
                self.print_soft_newline();
                self.print_indent();
            }
            item.print(self, ctx);
        }
    }

    fn print_expressions<T: GenExpr>(&mut self, items: &[T], precedence: Precedence, ctx: Context) {
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.print_comma();
                self.print_soft_space();
            }
            item.print_expr(self, precedence, ctx);
        }
    }

    fn get_identifier_reference_name(&self, reference: &IdentifierReference<'a>) -> &'a str {
        if let Some(mangler) = &self.mangler {
            if let Some(reference_id) = reference.reference_id.get() {
                if let Some(name) = mangler.get_reference_name(reference_id) {
                    // SAFETY: Hack the lifetime to be part of the allocator.
                    return unsafe { std::mem::transmute_copy(&name) };
                }
            }
        }
        reference.name.as_str()
    }

    fn get_binding_identifier_name(&self, ident: &BindingIdentifier<'a>) -> &'a str {
        if let Some(mangler) = &self.mangler {
            if let Some(symbol_id) = ident.symbol_id.get() {
                let name = mangler.get_symbol_name(symbol_id);
                // SAFETY: Hack the lifetime to be part of the allocator.
                return unsafe { std::mem::transmute_copy(&name) };
            }
        }
        ident.name.as_str()
    }

    fn print_space_before_operator(&mut self, next: Operator) {
        if self.prev_op_end != self.code.len() {
            return;
        }
        let Some(prev) = self.prev_op else { return };
        // "+ + y" => "+ +y"
        // "+ ++ y" => "+ ++y"
        // "x + + y" => "x+ +y"
        // "x ++ + y" => "x+++y"
        // "x + ++ y" => "x+ ++y"
        // "-- >" => "-- >"
        // "< ! --" => "<! --"
        let bin_op_add = Operator::Binary(BinaryOperator::Addition);
        let bin_op_sub = Operator::Binary(BinaryOperator::Subtraction);
        let un_op_pos = Operator::Unary(UnaryOperator::UnaryPlus);
        let un_op_pre_inc = Operator::Update(UpdateOperator::Increment);
        let un_op_neg = Operator::Unary(UnaryOperator::UnaryNegation);
        let un_op_pre_dec = Operator::Update(UpdateOperator::Decrement);
        let un_op_post_dec = Operator::Update(UpdateOperator::Decrement);
        let bin_op_gt = Operator::Binary(BinaryOperator::GreaterThan);
        let un_op_not = Operator::Unary(UnaryOperator::LogicalNot);
        if ((prev == bin_op_add || prev == un_op_pos)
            && (next == bin_op_add || next == un_op_pos || next == un_op_pre_inc))
            || ((prev == bin_op_sub || prev == un_op_neg)
                && (next == bin_op_sub || next == un_op_neg || next == un_op_pre_dec))
            || (prev == un_op_post_dec && next == bin_op_gt)
            || (prev == un_op_not && next == un_op_pre_dec && self.peek_nth(1) == Some('<'))
        {
            self.print_hard_space();
        }
    }

    #[inline]
    fn wrap<F: FnMut(&mut Self)>(&mut self, wrap: bool, mut f: F) {
        if wrap {
            self.print_char(b'(');
        }
        f(self);
        if wrap {
            self.print_char(b')');
        }
    }

    #[inline]
    fn wrap_quote<F: FnMut(&mut Self, u8)>(&mut self, mut f: F) {
        self.print_char(self.quote);
        f(self, self.quote);
        self.print_char(self.quote);
    }

    fn add_source_mapping(&mut self, position: u32) {
        if let Some(sourcemap_builder) = self.sourcemap_builder.as_mut() {
            sourcemap_builder.add_source_mapping(&self.code, position, None);
        }
    }

    fn add_source_mapping_for_name(&mut self, span: Span, name: &str) {
        if let Some(sourcemap_builder) = self.sourcemap_builder.as_mut() {
            sourcemap_builder.add_source_mapping_for_name(&self.code, span, name);
        }
    }
}

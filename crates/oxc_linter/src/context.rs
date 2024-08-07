#![allow(rustdoc::private_intra_doc_links)] // useful for intellisense
use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc};

use oxc_cfg::ControlFlowGraph;
use oxc_diagnostics::{OxcDiagnostic, Severity};
use oxc_semantic::{AstNodes, JSDocFinder, ScopeTree, Semantic, SymbolTable};
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::module_record::ModuleRecord;

use crate::{
    config::OxlintRules,
    disable_directives::{DisableDirectives, DisableDirectivesBuilder},
    fixer::{CompositeFix, Message, RuleFixer},
    javascript_globals::GLOBALS,
    AllowWarnDeny, OxlintConfig, OxlintEnv, OxlintGlobals, OxlintSettings,
};

#[derive(Clone)]
pub struct LintContext<'a> {
    semantic: Rc<Semantic<'a>>,

    /// Diagnostics reported by the linter.
    ///
    /// Contains diagnostics for all rules across all files.
    diagnostics: RefCell<Vec<Message<'a>>>,

    disable_directives: Rc<DisableDirectives<'a>>,

    /// Whether or not to apply code fixes during linting. Defaults to `false`.
    ///
    /// Set via the `--fix` CLI flag.
    fix: bool,

    file_path: Rc<Path>,

    eslint_config: Arc<OxlintConfig>,

    // states
    current_rule_name: &'static str,

    /// Current rule severity. Allows for user severity overrides, e.g.
    /// ```json
    /// // .oxlintrc.json
    /// {
    ///   "rules": {
    ///     "no-debugger": "error"
    ///   }
    /// }
    /// ```
    severity: Severity,
}

impl<'a> LintContext<'a> {
    /// # Panics
    /// If `semantic.cfg()` is `None`.
    pub fn new(file_path: Box<Path>, semantic: Rc<Semantic<'a>>) -> Self {
        const DIAGNOSTICS_INITIAL_CAPACITY: usize = 128;

        // We should always check for `semantic.cfg()` being `Some` since we depend on it and it is
        // unwrapped without any runtime checks after construction.
        assert!(
            semantic.cfg().is_some(),
            "`LintContext` depends on `Semantic::cfg`, Build your semantic with cfg enabled(`SemanticBuilder::with_cfg`)."
        );
        let disable_directives =
            DisableDirectivesBuilder::new(semantic.source_text(), semantic.trivias().clone())
                .build();
        Self {
            semantic,
            diagnostics: RefCell::new(Vec::with_capacity(DIAGNOSTICS_INITIAL_CAPACITY)),
            disable_directives: Rc::new(disable_directives),
            fix: false,
            file_path: file_path.into(),
            eslint_config: Arc::new(OxlintConfig::default()),
            current_rule_name: "",
            severity: Severity::Warning,
        }
    }

    /// Enable/disable automatic code fixes.
    #[must_use]
    pub fn with_fix(mut self, fix: bool) -> Self {
        self.fix = fix;
        self
    }

    #[must_use]
    pub fn with_eslint_config(mut self, eslint_config: &Arc<OxlintConfig>) -> Self {
        self.eslint_config = Arc::clone(eslint_config);
        self
    }

    #[must_use]
    pub fn with_rule_name(mut self, name: &'static str) -> Self {
        self.current_rule_name = name;
        self
    }

    #[must_use]
    pub fn with_severity(mut self, severity: AllowWarnDeny) -> Self {
        self.severity = Severity::from(severity);
        self
    }

    pub fn semantic(&self) -> &Rc<Semantic<'a>> {
        &self.semantic
    }

    pub fn cfg(&self) -> &ControlFlowGraph {
        #[allow(unsafe_code)]
        // SAFETY: `LintContext::new` is the only way to construct a `LintContext` and we always
        // assert the existence of control flow so it should always be `Some`.
        unsafe {
            self.semantic().cfg().unwrap_unchecked()
        }
    }

    pub fn disable_directives(&self) -> &DisableDirectives<'a> {
        &self.disable_directives
    }

    /// Source code of the file being linted.
    pub fn source_text(&self) -> &'a str {
        self.semantic().source_text()
    }

    /// Get a snippet of source text covered by the given [`Span`]. For details,
    /// see [`Span::source_text`].
    pub fn source_range(&self, span: Span) -> &'a str {
        span.source_text(self.semantic().source_text())
    }

    /// [`SourceType`] of the file currently being linted.
    pub fn source_type(&self) -> &SourceType {
        self.semantic().source_type()
    }

    /// Path to the file currently being linted.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Plugin settings
    pub fn settings(&self) -> &OxlintSettings {
        &self.eslint_config.settings
    }

    pub fn globals(&self) -> &OxlintGlobals {
        &self.eslint_config.globals
    }

    /// Runtime environments turned on/off by the user.
    ///
    /// Examples of environments are `builtin`, `browser`, `node`, etc.
    pub fn env(&self) -> &OxlintEnv {
        &self.eslint_config.env
    }

    pub fn rules(&self) -> &OxlintRules {
        &self.eslint_config.rules
    }

    pub fn env_contains_var(&self, var: &str) -> bool {
        if GLOBALS["builtin"].contains_key(var) {
            return true;
        }
        for env in self.env().iter() {
            if let Some(env) = GLOBALS.get(env) {
                if env.contains_key(var) {
                    return true;
                }
            }
        }
        false
    }

    /* Diagnostics */

    pub fn into_message(self) -> Vec<Message<'a>> {
        self.diagnostics.borrow().iter().cloned().collect::<Vec<_>>()
    }

    fn add_diagnostic(&self, message: Message<'a>) {
        if !self.disable_directives.contains(self.current_rule_name, message.span()) {
            let mut message = message;
            if message.error.severity != self.severity {
                message.error = message.error.with_severity(self.severity);
            }
            self.diagnostics.borrow_mut().push(message);
        }
    }

    /// Report a lint rule violation.
    ///
    /// Use [`LintContext::diagnostic_with_fix`] to provide an automatic fix.
    pub fn diagnostic(&self, diagnostic: OxcDiagnostic) {
        self.add_diagnostic(Message::new(diagnostic, None));
    }

    /// Report a lint rule violation and provide an automatic fix.
    ///
    /// The second argument is a [closure] that takes a [`RuleFixer`] and
    /// returns something that can turn into a [`CompositeFix`].
    ///
    /// [closure]: <https://doc.rust-lang.org/book/ch13-01-closures.html>
    pub fn diagnostic_with_fix<C, F>(&self, diagnostic: OxcDiagnostic, fix: F)
    where
        C: Into<CompositeFix<'a>>,
        F: FnOnce(RuleFixer<'_, 'a>) -> C,
    {
        if self.fix {
            let fixer = RuleFixer::new(self);
            let composite_fix: CompositeFix = fix(fixer).into();
            let fix = composite_fix.normalize_fixes(self.source_text());
            self.add_diagnostic(Message::new(diagnostic, Some(fix)));
        } else {
            self.diagnostic(diagnostic);
        }
    }

    /// AST nodes
    ///
    /// Shorthand for `self.semantic().nodes()`.
    pub fn nodes(&self) -> &AstNodes<'a> {
        self.semantic().nodes()
    }

    /// Scope tree
    ///
    /// Shorthand for `ctx.semantic().scopes()`.
    pub fn scopes(&self) -> &ScopeTree {
        self.semantic().scopes()
    }

    /// Symbol table
    ///
    /// Shorthand for `ctx.semantic().symbols()`.
    pub fn symbols(&self) -> &SymbolTable {
        self.semantic().symbols()
    }

    /// Imported modules and exported symbols
    ///
    /// Shorthand for `ctx.semantic().module_record()`.
    pub fn module_record(&self) -> &ModuleRecord {
        self.semantic().module_record()
    }

    /// JSDoc comments
    ///
    /// Shorthand for `ctx.semantic().jsdoc()`.
    pub fn jsdoc(&self) -> &JSDocFinder<'a> {
        self.semantic().jsdoc()
    }
}

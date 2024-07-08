mod expression;
mod import;
mod pattern;
#[cfg(test)]
mod tests;
mod typescript;

use std::cell::RefCell;
use std::sync::Arc;

use crate::analyse::TargetSupport;
use crate::build::Target;
use crate::codegen::TypeScriptDeclarations;
use crate::type_::{Type, TypeVar, PRELUDE_MODULE_NAME};
use crate::{
    ast::{CustomType, Function, Import, ModuleConstant, TypeAlias, *},
    docvec,
    line_numbers::LineNumbers,
    pretty::*,
};
use camino::Utf8Path;
use ecow::EcoString;
use expression::NeedsPureAnnotation;
use itertools::Itertools;

use self::import::{Imports, Member};

const INDENT: isize = 2;

pub const PRELUDE: &str = include_str!("../templates/prelude.mjs");
pub const PRELUDE_TS_DEF: &str = include_str!("../templates/prelude.d.mts");

pub type Output<'a> = Result<Document<'a>, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoCodegenTarget {
    Go,
    // TODO get rid of this?
    TypeScriptDeclarations,
}

#[derive(Debug)]
pub struct Generator<'a> {
    line_numbers: &'a LineNumbers,
    module: &'a TypedModule,
    tracker: UsageTracker,
    module_scope: im::HashMap<EcoString, usize>,
    current_module_name_segments_count: usize,
    target_support: TargetSupport,
    typescript: TypeScriptDeclarations,
}

impl<'a> Generator<'a> {
    pub fn new(
        line_numbers: &'a LineNumbers,
        module: &'a TypedModule,
        target_support: TargetSupport,
        typescript: TypeScriptDeclarations,
    ) -> Self {
        let current_module_name_segments_count = module.name.split('/').count();

        Self {
            current_module_name_segments_count,
            line_numbers,
            module,
            tracker: UsageTracker::default(),
            module_scope: Default::default(),
            target_support,
            typescript,
        }
    }

    fn type_reference(&self) -> Document<'a> {
        if self.typescript == TypeScriptDeclarations::None {
            return Document::Str("");
        }

        // Get the name of the module relative the directory (similar to basename)
        let module = self
            .module
            .name
            .as_str()
            .split('/')
            .last()
            .expect("JavaScript generator could not identify imported module name.");

        let name = Document::Str(module);

        docvec!["/// <reference types=\"./", name, ".d.mts\" />", line()]
    }

    pub fn compile(&mut self) -> Output<'a> {
        let type_reference = self.type_reference();

        // Determine what JavaScript imports we need to generate
        let mut imports = self.collect_imports();

        // Determine what names are defined in the module scope so we know to
        // rename any variables that are defined within functions using the same
        // names.
        self.register_module_definitions_in_scope();

        // Generate JavaScript code for each statement
        let statements = self.collect_definitions().into_iter().chain(
            self.module
                .definitions
                .iter()
                .flat_map(|s| self.statement(s)),
        );

        // Two lines between each statement
        let mut statements: Vec<_> =
            Itertools::intersperse(statements, Ok(lines(2))).try_collect()?;

        // Import any prelude functions that have been used

        if self.tracker.ok_used {
            self.register_prelude_usage(&mut imports, "Ok", None);
        };

        if self.tracker.error_used {
            self.register_prelude_usage(&mut imports, "Error", None);
        };

        if self.tracker.list_used {
            self.register_prelude_usage(&mut imports, "toList", None);
        };

        if self.tracker.prepend_used {
            self.register_prelude_usage(&mut imports, "prepend", Some("listPrepend"));
        };

        if self.tracker.custom_type_used {
            self.register_prelude_usage(&mut imports, "CustomType", Some("$CustomType"));
        };

        if self.tracker.make_error_used {
            self.register_prelude_usage(&mut imports, "makeError", None);
        };

        if self.tracker.int_remainder_used {
            self.register_prelude_usage(&mut imports, "remainderInt", None);
        };

        if self.tracker.float_division_used {
            self.register_prelude_usage(&mut imports, "divideFloat", None);
        };

        if self.tracker.int_division_used {
            self.register_prelude_usage(&mut imports, "divideInt", None);
        };

        if self.tracker.object_equality_used {
            self.register_prelude_usage(&mut imports, "isEqual", None);
        };

        if self.tracker.bit_array_literal_used {
            self.register_prelude_usage(&mut imports, "toBitArray", None);
        };

        if self.tracker.sized_integer_segment_used {
            self.register_prelude_usage(&mut imports, "sizedInt", None);
        };

        if self.tracker.string_bit_array_segment_used {
            self.register_prelude_usage(&mut imports, "stringBits", None);
        };

        if self.tracker.codepoint_bit_array_segment_used {
            self.register_prelude_usage(&mut imports, "codepointBits", None);
        };

        if self.tracker.float_bit_array_segment_used {
            self.register_prelude_usage(&mut imports, "float64Bits", None);
        };

        // Put it all together
        let package = if let Some((_, last_seg)) = self.module.name.rsplit_once("/") {
            last_seg
        } else {
            &self.module.name
        };

        let p = docvec![
            Document::String(format!("package {}", package)),
            line(),
            line()
        ];

        if imports.is_empty() && statements.is_empty() {
            Ok(docvec![p, type_reference, "export {}", line()])
        } else if imports.is_empty() {
            statements.push(line());
            Ok(docvec![p, type_reference, statements])
        } else if statements.is_empty() {
            Ok(docvec![
                p,
                type_reference,
                imports.into_doc(GoCodegenTarget::Go)
            ])
        } else {
            Ok(docvec![
                p,
                type_reference,
                imports.into_doc(GoCodegenTarget::Go),
                line(),
                statements,
                line()
            ])
        }
    }

    fn register_prelude_usage(
        &self,
        imports: &mut Imports<'a>,
        name: &'static str,
        alias: Option<&'static str>,
    ) {
        let path = self.import_path(&self.module.type_info.package, PRELUDE_MODULE_NAME);
        let member = Member {
            name: name.to_doc(),
            alias: alias.map(|a| a.to_doc()),
        };
        imports.register_module(path, [], [member]);
    }

    pub fn statement(&mut self, statement: &'a TypedDefinition) -> Option<Output<'a>> {
        match statement {
            Definition::TypeAlias(TypeAlias { .. }) => None,

            // Handled in collect_imports
            Definition::Import(Import { .. }) => None,

            // Handled in collect_definitions
            Definition::CustomType(CustomType { .. }) => None,

            Definition::ModuleConstant(ModuleConstant {
                publicity,
                name,
                value,
                ..
            }) => Some(self.module_constant(*publicity, name, value)),

            Definition::Function(function) => {
                // If there's an external JavaScript implementation then it will be imported,
                // so we don't need to generate a function definition.
                if function.has_external_for(Target::Go) {
                    return None;
                }

                // If the function does not support JavaScript then we don't need to generate
                // a function definition.
                if !function.implementations.supports(Target::Go) {
                    return None;
                }

                self.module_function(function)
            }
        }
    }

    fn custom_type_definition(
        &mut self,
        constructors: &'a [TypedRecordConstructor],
        publicity: Publicity,
        opaque: bool,
    ) -> Vec<Output<'a>> {
        // If there's no constructors then there's nothing to do here.
        if constructors.is_empty() {
            return vec![];
        }

        self.tracker.custom_type_used = true;
        constructors
            .iter()
            .map(|constructor| Ok(self.record_definition(constructor, publicity, opaque)))
            .collect()
    }

    fn record_definition(
        &self,
        constructor: &'a TypedRecordConstructor,
        publicity: Publicity,
        opaque: bool,
    ) -> Document<'a> {
        fn parameter((i, arg): (usize, &TypedRecordConstructorArg)) -> Document<'_> {
            arg.label
                .as_ref()
                .map(|s| maybe_escape_identifier_doc(s))
                .unwrap_or_else(|| Document::String(format!("x{i}")))
        }

        let head = if publicity.is_private() || opaque {
            "class "
        } else {
            "export class "
        };
        let head = docvec![head, &constructor.name, " extends $CustomType {"];

        if constructor.arguments.is_empty() {
            return head.append("}");
        };

        let parameters = join(
            constructor.arguments.iter().enumerate().map(parameter),
            break_(",", ", "),
        );

        let constructor_body = join(
            constructor.arguments.iter().enumerate().map(|(i, arg)| {
                let var = parameter((i, arg));
                match &arg.label {
                    None => docvec!["this[", i, "] = ", var, ";"],
                    Some(name) => docvec!["this.", name, " = ", var, ";"],
                }
            }),
            line(),
        );

        let class_body = docvec![
            line(),
            "constructor(",
            parameters,
            ") {",
            docvec![line(), "super();", line(), constructor_body].nest(INDENT),
            line(),
            "}",
        ]
        .nest(INDENT);

        docvec![head, class_body, line(), "}"]
    }

    fn collect_definitions(&mut self) -> Vec<Output<'a>> {
        self.module
            .definitions
            .iter()
            .flat_map(|statement| match statement {
                Definition::CustomType(CustomType {
                    publicity,
                    constructors,
                    opaque,
                    ..
                }) => self.custom_type_definition(constructors, *publicity, *opaque),

                Definition::Function(Function { .. })
                | Definition::TypeAlias(TypeAlias { .. })
                | Definition::Import(Import { .. })
                | Definition::ModuleConstant(ModuleConstant { .. }) => vec![],
            })
            .collect()
    }

    fn collect_imports(&mut self) -> Imports<'a> {
        let mut imports = Imports::new();

        for statement in &self.module.definitions {
            match statement {
                Definition::Import(Import {
                    module,
                    as_name,
                    unqualified_values: unqualified,
                    package,
                    ..
                }) => {
                    self.register_import(&mut imports, package, module, as_name, unqualified);
                }

                Definition::Function(Function {
                    name,
                    publicity,
                    externals,
                    ..
                }) if externals.iter().any(|e| e.target == Target::Go) => {
                    let external_javascript = externals
                        .iter()
                        .find(|e| e.target == Target::Go)
                        .expect("we know this is Some because we just checked");
                    self.register_external_function(
                        &mut imports,
                        *publicity,
                        name,
                        &external_javascript.module,
                        &external_javascript.function,
                    );
                }

                Definition::Function(Function { .. })
                | Definition::TypeAlias(TypeAlias { .. })
                | Definition::CustomType(CustomType { .. })
                | Definition::ModuleConstant(ModuleConstant { .. }) => (),
            }
        }

        imports
    }

    fn import_path(&self, package: &'a str, module: &'a str) -> String {
        // TODO: strip shared prefixed between current module and imported
        // module to avoid descending and climbing back out again
        if package == self.module.type_info.package || package.is_empty() {
            // Same package
            match self.current_module_name_segments_count {
                1 => format!("./{module}.mjs"),
                _ => {
                    let prefix = "../".repeat(self.current_module_name_segments_count - 1);
                    format!("{prefix}{module}.mjs")
                }
            }
        } else {
            // Different package
            let prefix = "../".repeat(self.current_module_name_segments_count);
            format!("{prefix}{package}/{module}.mjs")
        }
    }

    fn register_import(
        &mut self,
        imports: &mut Imports<'a>,
        package: &'a str,
        module: &'a str,
        as_name: &'a Option<(AssignName, SrcSpan)>,
        unqualified: &'a [UnqualifiedImport],
    ) {
        let get_name = |module: &'a str| {
            module
                .split('/')
                .last()
                .expect("JavaScript generator could not identify imported module name.")
        };

        let (discarded, module_name) = match as_name {
            None => (false, get_name(module)),
            Some((AssignName::Discard(_), _)) => (true, get_name(module)),
            Some((AssignName::Variable(name), _)) => (false, name.as_str()),
        };

        let module_name = format!("${module_name}");
        let path = self.import_path(package, module);
        let unqualified_imports = unqualified.iter().map(|i| {
            let alias = i.as_name.as_ref().map(|n| {
                self.register_in_scope(n);
                maybe_escape_identifier_doc(n)
            });
            let name = maybe_escape_identifier_doc(&i.name);
            Member { name, alias }
        });

        let aliases = if discarded { vec![] } else { vec![module_name] };
        imports.register_module(path, aliases, unqualified_imports);
    }

    fn register_external_function(
        &mut self,
        imports: &mut Imports<'a>,
        publicity: Publicity,
        name: &'a str,
        module: &'a str,
        fun: &'a str,
    ) {
        let needs_escaping = !is_usable_js_identifier(name);
        let member = Member {
            name: fun.to_doc(),
            alias: if name == fun && !needs_escaping {
                None
            } else if needs_escaping {
                Some(Document::String(escape_identifier(name)))
            } else {
                Some(name.to_doc())
            },
        };
        if publicity.is_importable() {
            imports.register_export(maybe_escape_identifier_string(name))
        }
        imports.register_module(module.to_string(), [], [member]);
    }

    fn module_constant(
        &mut self,
        publicity: Publicity,
        name: &'a str,
        value: &'a TypedConstant,
    ) -> Output<'a> {
        let head = if publicity.is_private() {
            "const "
        } else {
            "export const "
        };

        let mut needs_pure_annotation = NeedsPureAnnotation::No;
        let document =
            expression::constant_expression(&mut needs_pure_annotation, &mut self.tracker, value)?;

        let equals = match needs_pure_annotation {
            NeedsPureAnnotation::Yes => " = /* @__PURE__ */ ",
            NeedsPureAnnotation::No => " = ",
        };

        Ok(docvec![
            head,
            maybe_escape_identifier_doc(name),
            equals,
            document,
            ";",
        ])
    }

    fn register_in_scope(&mut self, name: &str) {
        let _ = self.module_scope.insert(name.into(), 0);
    }

    fn module_function(&mut self, function: &'a TypedFunction) -> Option<Output<'a>> {
        let argument_names = function
            .arguments
            .iter()
            .map(|arg| arg.names.get_variable_name())
            .collect();
        let mut generator = expression::Generator::new(
            self.module.name.clone(),
            self.line_numbers,
            function.name.clone(),
            argument_names,
            &mut self.tracker,
            self.module_scope.clone(),
        );
        let head = if function.publicity.is_private() {
            "func "
        } else {
            "export function "
        };

        let body = match generator.function_body(&function.body, function.arguments.as_slice()) {
            // No error, let's continue!
            Ok(body) => body,

            // There is an error coming from some expression that is not supported on JavaScript
            // and the target support is not enforced. In this case we do not error, instead
            // returning nothing which will cause no function to be generated.
            Err(error) if error.is_unsupported() && !self.target_support.is_enforced() => {
                return None
            }

            // Some other error case which will be returned to the user.
            Err(error) => return Some(Err(error)),
        };

        let _ = function.return_type;
        let document = docvec![
            head,
            maybe_escape_identifier_doc(function.name.as_str()),
            fun_args(function.arguments.as_slice(), generator.tail_recursion_used),
            " ",
            return_typ(&function.return_type),
            " {",
            docvec![line(), body].nest(INDENT).group(),
            line(),
            "}",
        ];
        Some(Ok(document))
    }

    fn register_module_definitions_in_scope(&mut self) {
        for statement in self.module.definitions.iter() {
            match statement {
                Definition::ModuleConstant(ModuleConstant { name, .. })
                | Definition::Function(Function { name, .. }) => self.register_in_scope(name),

                Definition::Import(Import {
                    unqualified_values: unqualified,
                    ..
                }) => unqualified
                    .iter()
                    .for_each(|unq_import| self.register_in_scope(unq_import.used_name())),

                Definition::TypeAlias(TypeAlias { .. })
                | Definition::CustomType(CustomType { .. }) => (),
            }
        }
    }
}

pub fn module(
    module: &TypedModule,
    line_numbers: &LineNumbers,
    path: &Utf8Path,
    src: &EcoString,
    target_support: TargetSupport,
    typescript: TypeScriptDeclarations,
) -> Result<String, crate::Error> {
    let document = Generator::new(line_numbers, module, target_support, typescript)
        .compile()
        .map_err(|error| crate::Error::Go {
            path: path.to_path_buf(),
            src: src.clone(),
            error,
        })?;
    Ok(document.to_pretty_string(80))
}

pub fn ts_declaration(
    module: &TypedModule,
    path: &Utf8Path,
    src: &EcoString,
) -> Result<String, crate::Error> {
    let document = typescript::TypeScriptGenerator::new(module)
        .compile()
        .map_err(|error| crate::Error::Go {
            path: path.to_path_buf(),
            src: src.clone(),
            error,
        })?;
    Ok(document.to_pretty_string(80))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Unsupported { feature: String, location: SrcSpan },
}

impl Error {
    /// Returns `true` if the error is [`Unsupported`].
    ///
    /// [`Unsupported`]: Error::Unsupported
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }
}

fn fun_args(args: &'_ [TypedArg], tail_recursion_used: bool) -> Document<'_> {
    let mut discards = 0;
    wrap_args(args.iter().map(|a| match a.get_variable_name() {
        None => {
            let doc = if discards == 0 {
                "_".to_doc()
            } else {
                Document::String(format!("_{discards}"))
            };
            discards += 1;
            doc
        }
        Some(name) if tail_recursion_used => Document::String(format!("loop${name}")),
        Some(name) => maybe_escape_identifier_doc(name),
    }))
}

fn return_typ(t: &Type) -> Document<'static> {
    match t {
        Type::Named {
            publicity: _,
            package: _,
            module,
            name,
            args: _,
        } => named_type(module, name),
        Type::Fn { args: _, retrn: _ } => todo!(),
        Type::Var { type_: type_a, .. } => type_var(&type_a.borrow()),
        Type::Tuple { elems } => tuple_return_type(elems),
    }
}

fn type_var(t: &TypeVar) -> Document<'static> {
    match t {
        TypeVar::Link {
            type_: another_type,
        } => return_typ(another_type),
        _ => todo!(),
    }
}

fn named_type(module: &str, name: &str) -> Document<'static> {
    match module {
        "gleam" => gleam_type(name),
        _ => todo!(),
    }
}

fn tuple_return_type(typs: &Vec<Arc<Type>>) -> Document<'static> {
    docvec![
        "(",
        join(typs.iter().map(|t| return_typ(t)), Document::Str(", ")),
        ")"
    ]
}

fn gleam_type(name: &str) -> Document<'static> {
    match name {
        "Int" => Document::Str("int"),
        "Bool" => Document::Str("bool"),
        "String" => Document::Str("string"),
        _ => todo!(),
    }
}

fn wrap_args<'a, I>(args: I) -> Document<'a>
where
    I: IntoIterator<Item = Document<'a>>,
{
    break_("", "")
        .append(join(args, break_(",", ", ")))
        .nest(INDENT)
        .append(break_("", ""))
        .surround("(", ")")
        .group()
}

fn wrap_object<'a>(
    items: impl IntoIterator<Item = (Document<'a>, Option<Document<'a>>)>,
) -> Document<'a> {
    let mut empty = true;
    let fields = items.into_iter().map(|(key, value)| {
        empty = false;
        match value {
            Some(value) => docvec![key, ": ", value],
            None => key.to_doc(),
        }
    });
    let fields = join(fields, break_(",", ", "));

    if empty {
        "{}".to_doc()
    } else {
        docvec![
            docvec!["{", break_("", " "), fields]
                .nest(INDENT)
                .append(break_("", " "))
                .group(),
            "}"
        ]
    }
}

fn try_wrap_object<'a>(items: impl IntoIterator<Item = (Document<'a>, Output<'a>)>) -> Output<'a> {
    let fields = items
        .into_iter()
        .map(|(key, value)| Ok(docvec![key, ": ", value?]));
    let fields: Vec<_> = Itertools::intersperse(fields, Ok(break_(",", ", "))).try_collect()?;

    Ok(docvec![
        docvec!["{", break_("", " "), fields]
            .nest(INDENT)
            .append(break_("", " "))
            .group(),
        "}"
    ])
}

fn is_usable_js_identifier(word: &str) -> bool {
    !matches!(
        word,
        // Keywords and reserved words
        // https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Lexical_grammar
        "await"
            | "arguments"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "eval"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            // `undefined` to avoid any unintentional overriding.
            | "undefined"
            // `then` to avoid a module that defines a `then` function being
            // used as a `thenable` in JavaScript when the module is imported
            // dynamically, which results in unexpected behaviour.
            // It is rather unfortunate that we have to do this.
            | "then"
    )
}

fn maybe_escape_identifier_string(word: &str) -> String {
    if is_usable_js_identifier(word) {
        word.to_string()
    } else {
        escape_identifier(word)
    }
}

fn escape_identifier(word: &str) -> String {
    format!("{word}$")
}

fn maybe_escape_identifier_doc(word: &str) -> Document<'_> {
    if is_usable_js_identifier(word) {
        word.to_doc()
    } else {
        Document::String(escape_identifier(word))
    }
}

#[derive(Debug, Default)]
pub(crate) struct UsageTracker {
    pub ok_used: bool,
    pub list_used: bool,
    pub prepend_used: bool,
    pub error_used: bool,
    pub int_remainder_used: bool,
    pub make_error_used: bool,
    pub custom_type_used: bool,
    pub int_division_used: bool,
    pub float_division_used: bool,
    pub object_equality_used: bool,
    pub bit_array_literal_used: bool,
    pub sized_integer_segment_used: bool,
    pub string_bit_array_segment_used: bool,
    pub codepoint_bit_array_segment_used: bool,
    pub float_bit_array_segment_used: bool,
}
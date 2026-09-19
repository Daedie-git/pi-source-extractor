use pi_source_extractor::{Extractor, InputFile, Request, cpp_parser, extract};

#[test]
fn functions_templates_constructors_operators_and_unicode() {
    let source = "// π\ntemplate <class T>\nT identity(T value) { return value; }\nstruct Box {\n  Box() {}\n  int& get() { return value; }\n  bool operator==(const Box& rhs) const { return value == rhs.value; }\n  int value;\n};\n";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert_eq!(
        out.units
            .iter()
            .map(|u| u.name.as_str())
            .collect::<Vec<_>>(),
        ["identity", "Box", "get", "operator=="]
    );
    assert_eq!((out.units[0].line, out.units[0].end), (2, 3));
    assert!(out.omitted.is_empty());
}

#[test]
fn recovery_does_not_accept_broken_bodies() {
    let source = "struct Label {};\nint valid(Label label = {}) { return 1; }\nint broken(Label label = {}) { return +; }\nstruct X { X() = default; };\n";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert!(out.units.iter().any(|u| u.name == "valid"));
    assert!(!out.units.iter().any(|u| u.name == "broken"));
    assert!(
        out.omitted
            .iter()
            .any(|d| d.unit.name == "broken" && d.reason == "parse_error")
    );
    assert!(
        out.omitted
            .iter()
            .any(|d| d.unit.name == "X" && d.reason == "no_body")
    );
}

#[test]
fn parallel_batches_preserve_order_and_match_serial() {
    let files = || {
        (0..8)
            .map(|i| InputFile {
                path: format!("file-{i}.cpp"),
                source: format!(
                    "// {}\nint f{i}() {{ return {i}; }}",
                    "padding".repeat(1500)
                ),
            })
            .collect()
    };
    let serial = Extractor::new(1).unwrap().process(Request {
        id: 1,
        files: files(),
    });
    let parallel = Extractor::new(4).unwrap().process(Request {
        id: 2,
        files: files(),
    });
    assert!(!serial.parallel);
    assert!(parallel.parallel);
    for (i, (a, b)) in serial.results.iter().zip(&parallel.results).enumerate() {
        assert_eq!(a.path, format!("file-{i}.cpp"));
        assert_eq!(a.path, b.path);
        assert_eq!(a.extraction, b.extraction);
    }
    let tiny = Extractor::new(4).unwrap().process(Request {
        id: 3,
        files: vec![
            InputFile {
                path: "a".into(),
                source: "int a() {}".into(),
            },
            InputFile {
                path: "b".into(),
                source: "int b() {}".into(),
            },
        ],
    });
    assert!(!tiny.parallel);
}

#[test]
fn parser_reuse_handles_empty_and_nested_input() {
    let mut parser = cpp_parser().unwrap();
    assert!(extract(&mut parser, "").unwrap().units.is_empty());
    let nested = format!(
        "int nested() {{ {}return 1;{} }}",
        "{".repeat(2000),
        "}".repeat(2000)
    );
    assert_eq!(extract(&mut parser, &nested).unwrap().units.len(), 1);
    assert_eq!(
        extract(&mut parser, "int after() { return 2; }")
            .unwrap()
            .units[0]
            .name,
        "after"
    );
}

#[test]
fn unicode_identifier_and_crlf_positions() {
    let result = extract(
        &mut cpp_parser().unwrap(),
        "// unicode\r\nint café() { return 1; }\r\n",
    )
    .unwrap();
    assert_eq!(result.units[0].name, "café");
    assert_eq!((result.units[0].line, result.units[0].end), (2, 2));
}

#[test]
fn many_macro_functions_have_exact_unaliased_positions() {
    let source: String = (0..500)
        .map(|i| {
            format!("TEST(Suite, case_{i}) {{\n    int value = {i};\n    consume(value);\n}}\n")
        })
        .collect();
    let mut parser = cpp_parser().unwrap();
    for _ in 0..5 {
        let output = extract(&mut parser, &source).unwrap();
        assert_eq!(output.units.len(), 500);
        for (i, unit) in output.units.iter().enumerate() {
            assert_eq!(unit.name, "TEST");
            assert_eq!((unit.line, unit.end), (i * 4 + 1, i * 4 + 4));
        }
    }
}

#[test]
fn configured_threads_are_capped_as_batches_grow_and_shrink() {
    let mut worker = Extractor::new(64).unwrap();
    for count in [3, 6, 2, 1, 0, 4, 4] {
        let response = worker.process(Request {
            id: count as u64,
            files: (0..count)
                .map(|i| InputFile {
                    path: format!("input-{i}.cpp"),
                    source: format!("// {}\nint f{i}() {{ return {i}; }}", "x".repeat(40000)),
                })
                .collect(),
        });
        assert_eq!(response.worker_threads, count);
        assert_eq!(response.parallel, count > 1);
        assert_eq!(response.results.len(), count);
        assert!(response.results.iter().all(|result| result.error.is_none()));
    }
    let tiny = worker.process(Request {
        id: 100,
        files: (0..3)
            .map(|i| InputFile {
                path: format!("small-{i}.cpp"),
                source: format!("int small{i}() {{}}"),
            })
            .collect(),
    });
    assert_eq!(tiny.worker_threads, 1);
    assert!(!tiny.parallel);
    assert!(Extractor::new(0).is_err());
}

#[test]
fn physical_core_default_is_used_as_the_concurrency_limit() {
    let cores = pi_source_extractor::default_threads();
    assert_eq!(cores, num_cpus::get_physical().max(1));
    let mut worker = Extractor::new(cores).unwrap();
    let response = worker.process(Request {
        id: 1,
        files: (0..3)
            .map(|i| InputFile {
                path: format!("default-{i}.cpp"),
                source: format!("// {}\nint default{i}() {{}}", "x".repeat(40000)),
            })
            .collect(),
    });
    assert_eq!(response.worker_threads, cores.min(3));
}

#[test]
fn scoped_references_ignore_text_receivers_signatures_and_nested_scopes() {
    let source = r#"
namespace One { int helper(int n) { return n; } }
namespace Two {
int helper(int n) { return n + 1; }
struct Box {
    void reserve(int n) {}
    void work(int signature = helper(8)) {
        // reserve(1); One::helper(2);
        const char* text = "reserve(1); One::helper(2);";
        this->reserve(4);
        other.reserve(5);
        helper(6);
        One::helper(7);
        auto nested = [] { reserve(9); };
    }
};
}
void Two::Box::outside() { this->reserve(1); helper(2); }
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let work = out.units.iter().find(|u| u.name == "work").unwrap();
    assert_eq!(work.qualified_name, "Two::Box::work");
    assert_eq!(
        work.scope
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect::<Vec<_>>(),
        [("namespace", "Two"), ("type", "Box")]
    );
    assert_eq!(
        work.references
            .iter()
            .map(|r| (r.name.as_str(), r.qualification.as_str(), r.arguments))
            .collect::<Vec<_>>(),
        [
            ("reserve", "this", 1),
            ("reserve", "unknown", 1),
            ("helper", "unqualified", 1),
            ("helper", "qualified", 1)
        ]
    );
    assert_eq!(work.references[3].qualifier, ["One"]);
    let outside = out.units.iter().find(|u| u.name == "outside").unwrap();
    assert_eq!(outside.qualified_name, "Two::Box::outside");
    assert_eq!(outside.scope, work.scope);
    assert_eq!(
        out.units
            .iter()
            .filter(|u| u.name == "helper")
            .map(|u| u.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["One::helper", "Two::helper"]
    );
}

#[test]
fn constructor_references_and_arity_preserve_default_argument_overloads() {
    let source = r#"
namespace N {
struct Item {
    Item(int one) {}
    Item(int a, int b, int c, int d = {}, int e = {}) {}
    static Item factory() { Item value {1, 2, 3, 4}; return value; }
    static Item returned() { return {1, 2, 3}; }
};
void work() { Item(1); Item{1}; Item x(1); Item empty; Item* pointer; }
}
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let ctors: Vec<_> = out
        .units
        .iter()
        .filter(|u| u.kind == "constructor")
        .collect();
    assert_eq!(ctors.len(), 2);
    assert_eq!((ctors[0].min_args, ctors[0].max_args), (Some(1), Some(1)));
    assert_eq!((ctors[1].min_args, ctors[1].max_args), (Some(3), Some(5)));
    assert_eq!(ctors[1].qualified_name, "N::Item::Item");
    let factory = out.units.iter().find(|u| u.name == "factory").unwrap();
    assert_eq!(factory.references.len(), 1);
    assert_eq!(
        (
            &*factory.references[0].kind,
            &*factory.references[0].name,
            factory.references[0].arguments
        ),
        ("construct", "Item", 4)
    );
    let returned = out.units.iter().find(|u| u.name == "returned").unwrap();
    assert_eq!(returned.references[0].arguments, 3);
    let work = out.units.iter().find(|u| u.name == "work").unwrap();
    assert_eq!(
        work.references
            .iter()
            .map(|r| (r.kind.as_str(), r.arguments))
            .collect::<Vec<_>>(),
        [("call", 1), ("construct", 1), ("construct", 0)]
    );
}

#[test]
fn uncertain_overloads_and_local_callable_shadowing_are_not_guessed() {
    let source = r#"
void helper(int n) {}
void helper(double n) {}
void variadic(int n, ...) {}
void f(void (*callback)(), int parameter) {
    int helper, other;
    helper(); other(); callback(); parameter();
}
void Unknown::method() { helper(1); }
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let helpers: Vec<_> = out.units.iter().filter(|u| u.name == "helper").collect();
    assert_eq!(helpers.len(), 2);
    assert_eq!(helpers[0].qualified_name, helpers[1].qualified_name);
    assert_eq!(helpers[0].min_args, helpers[1].min_args);
    assert_eq!(
        out.units
            .iter()
            .find(|u| u.name == "variadic")
            .unwrap()
            .max_args,
        None
    );
    let f = out.units.iter().find(|u| u.name == "f").unwrap();
    assert_eq!(f.references.iter().filter(|r| r.kind == "call").count(), 4);
    assert!(
        f.references
            .iter()
            .filter(|r| r.kind == "call")
            .all(|r| r.qualification == "unknown")
    );
    assert_eq!(
        out.units.iter().find(|u| u.name == "method").unwrap().scope[0].kind,
        "unknown"
    );
}

#[test]
fn constructor_initializer_calls_are_kept_but_not_field_initializers() {
    let source = "struct X { int member; X(int n) : member(helper(n)) {} };";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert_eq!(out.units[0].references.len(), 1);
    assert_eq!(out.units[0].references[0].name, "helper");
}

#[test]
fn declarations_imports_and_unknown_owners_block_unsafe_fallthrough() {
    let source = r#"
void helper() {}
struct A { void helper(); void run() { helper(); this->helper(); } };
namespace N { void other(int n) {} void other(double n); }
void qualified() { N::other(1); }
void imported() { using N::other; other(1); }
void Unknown::owner() { helper(); }
namespace Aliased { struct A { static void help() {} }; }
void alias() { using A = Aliased::A; A::help(); }
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    for name in ["run", "qualified", "imported", "owner", "alias"] {
        let unit = out.units.iter().find(|u| u.name == name).unwrap();
        assert!(!unit.references.is_empty(), "{name}");
        assert!(
            unit.references.iter().all(|r| r.qualification == "unknown"),
            "{name}: {:?}",
            unit.references
        );
    }
}

#[test]
fn explicit_static_calls_and_global_names_retain_qualification() {
    let source = "namespace N { struct Builder { static void build() {} }; void helper() {} } void f() { N::Builder::build(); ::N::helper(); }";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let f = out.units.iter().find(|u| u.name == "f").unwrap();
    assert_eq!(f.references.len(), 2);
    assert_eq!(f.references[0].qualifier, ["N", "Builder"]);
    assert_eq!(f.references[1].qualifier, ["", "N"]);
    assert!(f.references.iter().all(|r| r.qualification == "qualified"));
}

#[test]
fn unqualified_construction_respects_declaration_only_constructor_overloads() {
    let source =
        "struct Item { Item(int n) {} Item(double n); }; void f() { Item value{1.5}; Item(1.5); }";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let f = out.units.iter().find(|u| u.name == "f").unwrap();
    assert_eq!(f.references.len(), 2);
    assert!(
        f.references
            .iter()
            .all(|r| r.name == "Item" && r.qualification == "unknown")
    );
}

#[test]
fn annotation_prefix_recovery_preserves_original_metadata_and_ranges() {
    let source = r#"namespace Demo {
int helper(int value) { return value; }
struct Item {};
struct Box {
    EXPORT_HINT void set(int value) { helper(value); }
    template<class T>
    [[nodiscard]] PROJECT_FAST_INLINE Result convert(T value) { return helper(value); }
    EXPORT_HINT const Item& reference() { return item; }
    EXPORT_HINT Item* pointer() { return &item; }
    static EXPORT_HINT int number(int value = 1) { return helper(value); }
    Item item;
};
}
"#;
    let output = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert!(
        output.parse_has_error,
        "Original syntax errors remain visible"
    );
    assert!(output.omitted.is_empty(), "{:?}", output.omitted);
    let names: Vec<_> = output
        .recovered
        .iter()
        .map(|d| d.unit.name.as_str())
        .collect();
    assert_eq!(names, ["set", "convert", "reference", "pointer", "number"]);
    assert!(
        output
            .recovered
            .iter()
            .all(|d| d.reason == "annotation_prefix_signature")
    );
    let normalized = source
        .replace("EXPORT_HINT", &" ".repeat("EXPORT_HINT".len()))
        .replace(
            "PROJECT_FAST_INLINE",
            &" ".repeat("PROJECT_FAST_INLINE".len()),
        );
    let clean = extract(&mut cpp_parser().unwrap(), &normalized).unwrap();
    assert!(!clean.parse_has_error);
    assert_eq!(
        output.units, clean.units,
        "Scopes, arity and references must match clean syntax"
    );
    let convert = output.units.iter().find(|u| u.name == "convert").unwrap();
    assert_eq!((convert.line, convert.end), (6, 7));
    assert_eq!(convert.qualified_name, "Demo::Box::convert");
    assert_eq!(convert.references[0].name, "helper");
}

#[test]
fn annotation_recovery_handles_default_braces_crlf_unicode_and_multiline_prefixes() {
    let source = "// π\r\nstruct Label {};\r\nANNOTATION\r\nint café(Label label = {}) { return 1; }\r\nint after() { return 2; }\r\n";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let unit = out.units.iter().find(|u| u.name == "café").unwrap();
    assert_eq!((unit.line, unit.end), (3, 4));
    assert_eq!((unit.min_args, unit.max_args), (Some(0), Some(1)));
    assert_eq!(out.recovered.len(), 1);
    assert_eq!(out.recovered[0].reason, "annotation_prefix_signature");
    assert_eq!(out.units.last().unwrap().name, "after");
}

#[test]
fn annotation_recovery_rejects_broken_bodies_parameters_and_unrecognized_prefixes() {
    let source = r#"ANNOTATION int valid() { return 1; }
ANNOTATION int broken_body() { return +; }
ANNOTATION int broken_parameter(int ???) { return 2; }
ANNOTATION int broken_return ??? bad() { return 3; }
ordinary_type int ambiguous() { return 4; }
XX int short_prefix() { return 5; }
int neighbor() { return 6; }
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert_eq!(
        out.units
            .iter()
            .map(|u| u.name.as_str())
            .collect::<Vec<_>>(),
        ["valid", "neighbor"]
    );
    assert_eq!(out.recovered.len(), 1);
    assert!(out.omitted.iter().any(|d| d.unit.name == "broken_body"));
    assert!(
        out.omitted
            .iter()
            .any(|d| d.unit.name == "broken_parameter")
    );
    assert!(out.omitted.iter().any(|d| d.unit.name == "ambiguous"));
}

#[test]
fn annotation_recovery_does_not_repair_invalid_template_parameters() {
    let source = "template <class T ???>\nANNOTATION int invalid(T x) { return 1; }\nANNOTATION int valid() { return 2; }\n";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert!(!out.units.iter().any(|u| u.name == "invalid"));
    assert!(out.units.iter().any(|u| u.name == "valid"));
}

#[test]
fn annotation_recovery_keeps_same_line_neighbors_and_parallel_results_identical() {
    let source = "int before() { return 0; } ANNOTATION int middle() { return before(); } int after() { return 2; }\n";
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    assert_eq!(
        out.units
            .iter()
            .map(|u| u.name.as_str())
            .collect::<Vec<_>>(),
        ["before", "middle", "after"]
    );
    assert_eq!(out.recovered[0].unit.name, "middle");
    assert!(out.units.iter().all(|u| u.line == 1 && u.end == 1));
    let files = || {
        (0..4)
            .map(|i| InputFile {
                path: format!("macro-{i}.cpp"),
                source: format!("// {}\n{source}", "x".repeat(17000)),
            })
            .collect()
    };
    let serial = Extractor::new(1).unwrap().process(Request {
        id: 1,
        files: files(),
    });
    let parallel = Extractor::new(4).unwrap().process(Request {
        id: 2,
        files: files(),
    });
    assert!(parallel.parallel);
    for (a, b) in serial.results.iter().zip(parallel.results.iter()) {
        assert_eq!(a.path, b.path);
        assert_eq!(a.extraction, b.extraction);
    }
}

#[test]
fn annotation_recovery_uses_validated_return_type_and_qualified_signature() {
    let source = r#"namespace Library { struct Item { Item() {} }; }
struct Result { Result() {} };
namespace Demo {
    ANNOTATION Library::Item value(int n, int extra = 0) { helper(n); return {}; }
    template<class T>
    ANNOTATION Library::List<T> values(T n) { return make(n); }
    ANNOTATION Result empty() { return {}; }
    ANNOTATION Library::Item broken(int ???) { return {}; }
    struct Box { ANNOTATION Library::Item member() { return make(); } };
}
"#;
    let out = extract(&mut cpp_parser().unwrap(), source).unwrap();
    let clean = extract(
        &mut cpp_parser().unwrap(),
        &source.replace("ANNOTATION", "          "),
    )
    .unwrap();
    assert_eq!(out.units, clean.units);
    assert_eq!(
        out.recovered
            .iter()
            .map(|d| d.unit.name.as_str())
            .collect::<Vec<_>>(),
        ["value", "values", "empty", "member"]
    );
    let value = out.units.iter().find(|u| u.name == "value").unwrap();
    assert_eq!(value.qualified_name, "Demo::value");
    assert_eq!((value.min_args, value.max_args), (Some(1), Some(2)));
    assert_eq!(
        value
            .references
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["helper", "Item"]
    );
    let empty = out.units.iter().find(|u| u.name == "empty").unwrap();
    assert_eq!(empty.references[0].name, "Result");
    assert!(!out.units.iter().any(|u| u.name == "broken"));
}

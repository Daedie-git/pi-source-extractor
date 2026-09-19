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

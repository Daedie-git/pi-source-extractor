//! Conservative syntax metadata, not C++ name lookup or type inference.
use crate::{Unit, visit};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scope {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Reference {
    pub kind: String,
    pub name: String,
    pub qualification: String,
    pub qualifier: Vec<String>,
    pub arguments: usize,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn parts(value: &str) -> Option<Vec<String>> {
    let result: Vec<_> = value
        .split("::")
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    result
        .iter()
        .all(|s| !s.is_empty() && s.chars().all(|c| c == '_' || c.is_alphanumeric()))
        .then_some(result)
}

fn lexical_scope(node: Node<'_>, source: &str) -> Vec<Scope> {
    let mut groups = vec![];
    let mut parent = node.parent();
    while let Some(p) = parent {
        let kind = match p.kind() {
            "namespace_definition" => Some("namespace"),
            "class_specifier" | "struct_specifier" | "union_specifier" => Some("type"),
            _ => None,
        };
        if let Some(kind) = kind {
            let names = p
                .child_by_field_name("name")
                .and_then(|n| parts(text(n, source)))
                .unwrap_or_else(|| vec![format!("(anonymous@{})", p.start_byte())]);
            groups.push(
                names
                    .into_iter()
                    .map(|name| Scope {
                        kind: kind.into(),
                        name,
                    })
                    .collect::<Vec<_>>(),
            );
        }
        parent = p.parent();
    }
    groups.into_iter().rev().flatten().collect()
}

pub struct ScopeIndex {
    kinds: HashMap<String, String>,
    blocked: HashMap<String, HashSet<String>>,
    uncertain: HashSet<String>,
    defined_members: HashMap<String, HashSet<String>>,
}

fn scope_path(scope: &[Scope]) -> String {
    scope
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

fn binding_name(mut node: Node<'_>, source: &str) -> Option<String> {
    loop {
        if let Some(next) = node.child_by_field_name("declarator") {
            node = next;
            continue;
        }
        if node.kind() == "parenthesized_declarator"
            && let Some(next) = node.named_child(0)
        {
            node = next;
            continue;
        }
        return matches!(
            node.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        )
        .then(|| text(node, source).to_owned());
    }
}

pub fn scope_index(root: Node<'_>, source: &str) -> ScopeIndex {
    let mut index = ScopeIndex {
        kinds: HashMap::new(),
        blocked: HashMap::new(),
        uncertain: HashSet::new(),
        defined_members: HashMap::new(),
    };
    visit(root, |node| {
        if !matches!(
            node.kind(),
            "declaration"
                | "field_declaration"
                | "type_definition"
                | "alias_declaration"
                | "namespace_alias_definition"
                | "using_declaration"
                | "base_class_clause"
        ) {
            return;
        }
        let mut p = node.parent();
        while let Some(ancestor) = p {
            if matches!(ancestor.kind(), "function_definition" | "lambda_expression") {
                return;
            }
            p = ancestor.parent();
        }
        let path = scope_path(&lexical_scope(node, source));
        match node.kind() {
            "declaration" | "field_declaration" | "type_definition" => {
                let mut cursor = node.walk();
                for d in node.children_by_field_name("declarator", &mut cursor) {
                    if let Some(name) = binding_name(d, source) {
                        index.blocked.entry(path.clone()).or_default().insert(name);
                    }
                }
            }
            "alias_declaration" | "namespace_alias_definition" => {
                if let Some(n) = node.child_by_field_name("name") {
                    index
                        .blocked
                        .entry(path)
                        .or_default()
                        .insert(text(n, source).to_owned());
                }
            }
            "using_declaration" => {
                index.uncertain.insert(path);
            }
            "base_class_clause" => {
                index.uncertain.insert(path);
            }
            _ => {}
        }
    });
    visit(root, |node| {
        let kind = match node.kind() {
            "namespace_definition" => "namespace",
            "class_specifier" | "struct_specifier" | "union_specifier" => "type",
            _ => return,
        };
        let Some(names) = node
            .child_by_field_name("name")
            .and_then(|n| parts(text(n, source)))
        else {
            return;
        };
        let mut path: Vec<_> = lexical_scope(node, source)
            .into_iter()
            .map(|s| s.name)
            .collect();
        for name in names {
            path.push(name);
            index.kinds.insert(path.join("::"), kind.into());
        }
    });
    visit(root, |node| {
        if node.kind() != "function_definition" {
            return;
        }
        let scope = lexical_scope(node, source);
        if !scope.last().is_some_and(|s| s.kind == "type") {
            return;
        }
        let name = node
            .child_by_field_name("declarator")
            .and_then(function_declarator)
            .and_then(|d| d.child_by_field_name("declarator"))
            .and_then(|n| parts(text(n, source)));
        if let Some(names) = name
            && names.len() == 1
        {
            index
                .defined_members
                .entry(scope_path(&scope))
                .or_default()
                .insert(names[0].clone());
        }
    });
    index
}

fn function_declarator(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if node.kind() == "function_declarator" {
            return Some(node);
        }
        node = node.child_by_field_name("declarator")?;
    }
}

// Avoid assigning nested functions, local classes, or lambdas to their enclosing unit.
fn body_visit(root: Node<'_>, mut f: impl FnMut(Node<'_>)) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let skip = matches!(
            node.kind(),
            "lambda_expression"
                | "function_definition"
                | "class_specifier"
                | "struct_specifier"
                | "union_specifier"
        );
        if !skip {
            f(node);
        }
        if !skip && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn arity(declarator: Option<Node<'_>>) -> (Option<usize>, Option<usize>) {
    let Some(parameters) = declarator.and_then(|n| n.child_by_field_name("parameters")) else {
        return (None, None);
    };
    let mut cursor = parameters.walk();
    let mut min = 0;
    let mut max = 0;
    for child in parameters.children(&mut cursor) {
        match child.kind() {
            "(" | ")" | "," | "comment" => {}
            "parameter_declaration" => {
                min += 1;
                max += 1;
            }
            "optional_parameter_declaration" => max += 1,
            _ => return (None, None),
        }
    }
    (Some(min), Some(max))
}

fn arguments(node: Node<'_>) -> usize {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .count()
}

fn reference(name: Node<'_>, args: Node<'_>, kind: &str, source: &str) -> Option<Reference> {
    let raw = text(name, source);
    let absolute = raw.trim_start().starts_with("::");
    let mut names = parts(raw.trim_start_matches("::"))?;
    let name = names.pop()?;
    if absolute {
        names.insert(0, String::new());
    }
    Some(Reference {
        kind: kind.into(),
        name,
        qualification: if names.is_empty() {
            "unqualified"
        } else {
            "qualified"
        }
        .into(),
        qualifier: names,
        arguments: arguments(args),
    })
}

pub fn unit(
    node: Node<'_>,
    source: &str,
    index: &ScopeIndex,
    name: String,
    line: usize,
    end: usize,
) -> Unit {
    let declarator = node
        .child_by_field_name("declarator")
        .and_then(function_declarator);
    let mut scope = lexical_scope(node, source);
    if let Some(raw_name) = declarator.and_then(|d| d.child_by_field_name("declarator"))
        && let Some(mut names) = parts(text(raw_name, source).trim_start_matches("::"))
    {
        names.pop();
        let mut path: Vec<_> = scope.iter().map(|s| s.name.clone()).collect();
        if text(raw_name, source).starts_with("::") {
            scope.clear();
            path.clear();
        }
        for name in names {
            path.push(name.clone());
            scope.push(Scope {
                kind: index
                    .kinds
                    .get(&path.join("::"))
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                name,
            });
        }
    }
    if declarator
        .and_then(|d| d.child_by_field_name("declarator"))
        .is_some_and(|raw| {
            raw.kind() == "qualified_identifier"
                && parts(text(raw, source).trim_start_matches("::")).is_none()
        })
    {
        scope.push(Scope {
            kind: "unknown".into(),
            name: format!("(unresolved@{})", node.start_byte()),
        });
    }
    let qualified_name = scope
        .iter()
        .map(|s| s.name.as_str())
        .chain(std::iter::once(name.as_str()))
        .collect::<Vec<_>>()
        .join("::");
    let kind = if node.child_by_field_name("type").is_none()
        && scope
            .last()
            .is_some_and(|s| s.kind == "type" && s.name == name)
    {
        "constructor"
    } else {
        "function"
    }
    .to_owned();
    let (mut min_args, mut max_args) = arity(declarator);
    if declarator
        .and_then(|d| d.child_by_field_name("parameters"))
        .is_some_and(|p| text(p, source).trim() == "(void)")
    {
        min_args = Some(0);
        max_args = Some(0);
    }
    let mut references = vec![];
    if let Some(body) = node.child_by_field_name("body") {
        let mut roots = vec![body];
        let mut cursor = node.walk();
        roots.extend(
            node.named_children(&mut cursor)
                .filter(|n| n.kind() == "field_initializer_list"),
        );
        let mut shadows = HashSet::new();
        let mut uncertain = scope.iter().any(|s| s.kind == "unknown");
        for i in 0..=scope.len() {
            let path = scope_path(&scope[..i]);
            if let Some(names) = index.blocked.get(&path) {
                shadows.extend(names.iter().cloned());
            }
            uncertain |= index.uncertain.contains(&path);
        }
        let mut local_uncertain = false;
        let mut record_binding = |n: Node<'_>| {
            if matches!(
                n.kind(),
                "using_declaration" | "alias_declaration" | "type_definition"
            ) {
                local_uncertain = true;
            }
            if matches!(
                n.kind(),
                "parameter_declaration"
                    | "optional_parameter_declaration"
                    | "init_declarator"
                    | "declaration"
            ) {
                let mut cursor = n.walk();
                for mut d in n.children_by_field_name("declarator", &mut cursor) {
                    loop {
                        if let Some(next) = d.child_by_field_name("declarator") {
                            d = next;
                            continue;
                        }
                        if d.kind() == "parenthesized_declarator"
                            && let Some(next) = d.named_child(0)
                        {
                            d = next;
                            continue;
                        }
                        break;
                    }
                    if d.kind() == "identifier" {
                        shadows.insert(text(d, source).to_owned());
                    }
                }
            }
        };
        if let Some(params) = declarator.and_then(|d| d.child_by_field_name("parameters")) {
            visit(params, &mut record_binding);
        }
        for root in &roots {
            body_visit(*root, &mut record_binding);
        }
        let mut seen = HashSet::new();
        for root in roots {
            body_visit(root, |n| {
                let candidate = match n.kind() {
                    "call_expression" => {
                        let Some(function) = n.child_by_field_name("function") else {
                            return;
                        };
                        let Some(args) = n.child_by_field_name("arguments") else {
                            return;
                        };
                        if function.kind() == "field_expression" {
                            let Some(field) = function.child_by_field_name("field") else {
                                return;
                            };
                            reference(field, args, "call", source).map(|mut r| {
                                r.qualification = if function
                                    .child_by_field_name("argument")
                                    .is_some_and(|a| a.kind() == "this")
                                {
                                    "this"
                                } else {
                                    "unknown"
                                }
                                .into();
                                r
                            })
                        } else {
                            reference(function, args, "call", source)
                        }
                    }
                    "compound_literal_expression" => n
                        .child_by_field_name("type")
                        .zip(n.child_by_field_name("value"))
                        .and_then(|(ty, args)| reference(ty, args, "construct", source)),
                    "init_declarator" => {
                        let Some(args) = n
                            .child_by_field_name("value")
                            .filter(|v| matches!(v.kind(), "initializer_list" | "argument_list"))
                        else {
                            return;
                        };
                        n.parent()
                            .and_then(|p| p.child_by_field_name("type"))
                            .and_then(|ty| reference(ty, args, "construct", source))
                    }
                    "declaration" => {
                        // Only a direct value declaration default-constructs a type.
                        let mut cursor = n.walk();
                        let plain = n
                            .children_by_field_name("declarator", &mut cursor)
                            .any(|d| d.kind() == "identifier");
                        if plain {
                            n.child_by_field_name("type")
                                .and_then(|ty| reference(ty, n, "construct", source))
                                .map(|mut r| {
                                    r.arguments = 0;
                                    r
                                })
                        } else {
                            None
                        }
                    }
                    "return_statement" => {
                        let mut cursor = n.walk();
                        n.named_children(&mut cursor)
                            .find(|c| c.kind() == "initializer_list")
                            .and_then(|args| {
                                node.child_by_field_name("type")
                                    .and_then(|ty| reference(ty, args, "construct", source))
                            })
                    }
                    _ => None,
                };
                if let Some(mut r) = candidate {
                    // A definite member in this class hides names imported by an
                    // outer namespace. Same-scope ambiguity, local bindings and
                    // unknown receivers remain conservative.
                    let own_scope = scope_path(&scope);
                    let known_member = matches!(r.qualification.as_str(), "unqualified" | "this")
                        && scope.last().is_some_and(|s| s.kind == "type")
                        && !scope.iter().any(|s| s.kind == "unknown")
                        && !index.uncertain.contains(&own_scope)
                        && index
                            .defined_members
                            .get(&own_scope)
                            .is_some_and(|names| names.contains(&r.name));
                    let mut blocked = (local_uncertain || (uncertain && !known_member))
                        && r.qualification != "unknown";
                    if matches!(r.qualification.as_str(), "unqualified" | "this") {
                        blocked |= shadows.contains(&r.name);
                        // A type's declaration-only constructor overload can shadow its
                        // extracted definition even when the construction is unqualified.
                        for i in 0..=scope.len() {
                            let constructor_path = scope[..i]
                                .iter()
                                .map(|s| s.name.as_str())
                                .chain(std::iter::once(r.name.as_str()))
                                .collect::<Vec<_>>()
                                .join("::");
                            blocked |= index
                                .blocked
                                .get(&constructor_path)
                                .is_some_and(|names| names.contains(&r.name));
                        }
                    } else if r.qualification == "qualified" {
                        let absolute = r.qualifier.first().is_some_and(|q| q.is_empty());
                        if !absolute {
                            blocked |= r.qualifier.first().is_some_and(|q| shadows.contains(q));
                        }
                        for i in 0..=if absolute { 0 } else { scope.len() } {
                            let path = scope[..i]
                                .iter()
                                .map(|s| s.name.as_str())
                                .chain(
                                    r.qualifier
                                        .iter()
                                        .filter(|q| !q.is_empty())
                                        .map(String::as_str),
                                )
                                .collect::<Vec<_>>()
                                .join("::");
                            blocked |= index.uncertain.contains(&path)
                                || index
                                    .blocked
                                    .get(&path)
                                    .is_some_and(|names| names.contains(&r.name));
                            let constructor_path = if path.is_empty() {
                                r.name.clone()
                            } else {
                                format!("{path}::{}", r.name)
                            };
                            blocked |= index
                                .blocked
                                .get(&constructor_path)
                                .is_some_and(|names| names.contains(&r.name));
                        }
                    }
                    if blocked {
                        r.qualification = "unknown".into();
                    }
                    if seen.insert(r.clone()) {
                        references.push(r);
                    }
                }
            });
        }
    }
    Unit {
        name,
        line,
        end,
        scope,
        qualified_name,
        kind,
        min_args,
        max_args,
        references,
    }
}

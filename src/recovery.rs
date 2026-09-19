//! Narrow syntactic recovery for unexpanded, object-like annotation prefixes.
//! Uppercase spelling is a heuristic, not evidence of a macro definition.
use crate::{Unit, metadata, name_of, recoverable_signature, visit};
use std::collections::HashMap;
use std::ops::Range;
use tree_sitter::{Node, Parser, Tree};

struct Candidate {
    function: (usize, usize),
    template_start: Option<usize>,
    line: usize,
    end: usize,
    prefix: Range<usize>,
}

fn candidate(node: Node<'_>, source: &str) -> Option<(Range<usize>, Candidate)> {
    if node.kind() != "function_definition" || !node.has_error() {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    let declarator = node.child_by_field_name("declarator")?;
    let prefix = node.child_by_field_name("type")?;
    let name = &source[prefix.byte_range()];
    if body.has_error()
        || body.kind() != "compound_statement"
        || prefix.kind() != "type_identifier"
        || name.len() < 3
        || !name.bytes().any(|b| b.is_ascii_uppercase())
        || !name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        || prefix.end_byte() > declarator.start_byte()
    {
        return None;
    }
    let mut signature_error = false;
    let mut valid = true;
    visit(node, |issue| {
        if !issue.is_error() && !issue.is_missing() {
            return;
        }
        let in_signature =
            issue.start_byte() >= prefix.end_byte() && issue.end_byte() <= body.start_byte();
        signature_error |= in_signature;
        valid &= in_signature;
    });
    (valid && signature_error).then_some((
        body.byte_range(),
        Candidate {
            function: (node.start_byte(), node.end_byte()),
            template_start: node
                .parent()
                .filter(|p| p.kind() == "template_declaration")
                .map(|p| p.start_byte()),
            line: node
                .parent()
                .filter(|p| p.kind() == "template_declaration")
                .unwrap_or(node)
                .start_position()
                .row
                + 1,
            end: node.end_position().row + 1,
            prefix: prefix.byte_range(),
        },
    ))
}

pub(super) fn annotation_prefixes(
    parser: &mut Parser,
    original: &Tree,
    source: &str,
    scopes: &metadata::ScopeIndex,
) -> Result<HashMap<(usize, usize), Unit>, String> {
    let mut candidates = HashMap::new();
    if !original.root_node().has_error() {
        return Ok(HashMap::new());
    }
    visit(original.root_node(), |node| {
        if let Some((body, candidate)) = candidate(node, source) {
            candidates.insert((body.start, body.end), candidate);
        }
    });
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    // One extra parse per affected file, not one per function. Only nominated
    // ASCII prefix bytes change, preserving every source offset and newline.
    let mut normalized = source.as_bytes().to_vec();
    for candidate in candidates.values() {
        normalized[candidate.prefix.clone()].fill(b' ');
    }
    let tree = parser.parse(&normalized, None).ok_or("parse cancelled")?;
    let mut recovered = HashMap::new();
    visit(tree.root_node(), |node| {
        if node.kind() != "function_definition" {
            return;
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let Some(candidate) = candidates.get(&(body.start_byte(), body.end_byte())) else {
            return;
        };
        let template = node.parent().filter(|p| p.kind() == "template_declaration");
        let Some(declarator) = node.child_by_field_name("declarator") else {
            return;
        };
        let Some(return_type) = node.child_by_field_name("type") else {
            return;
        };
        // A nominated body must still belong to the same complete function.
        // Qualified return types can corrupt the original declarator, so validate
        // the normalized signature geometrically instead of trusting that field.
        if node.start_byte() < candidate.function.0
            || node.start_byte() >= body.start_byte()
            || node.end_byte() != candidate.function.1
            || template.map(|p| p.start_byte()) != candidate.template_start
            || return_type.start_byte() < candidate.prefix.end
            || return_type.end_byte() > body.start_byte()
            || declarator.start_byte() < candidate.prefix.end
            || declarator.end_byte() > body.start_byte()
        {
            return;
        }
        let signature = template.unwrap_or(node);
        if !signature.has_error() || recoverable_signature(signature, body) {
            // Offsets are unchanged: use original bytes and original declaration
            // lookup, but the validated tree for name, arity and return type.
            recovered.insert(
                candidate.function,
                metadata::unit(
                    node,
                    source,
                    scopes,
                    name_of(declarator, source),
                    candidate.line,
                    candidate.end,
                ),
            );
        }
    });
    Ok(recovered)
}

//! The record display DSL: each model carries named blocks of layout nodes.
//! `detail` is the reserved record-page block; any other name is a reusable
//! fragment (for list cards or `resource` items on other models' pages).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{FieldType, Model, Order, Problem, View};

/// Breakpoint minimum widths, shared with the admin stylesheet.
pub const BREAKPOINTS: [(&str, u32); 5] = [
    ("default", 0),
    ("sm", 640),
    ("md", 768),
    ("lg", 1024),
    ("xl", 1280),
];

/// A named display block: one node, or a list rendered as an implicit stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DisplayBlock {
    Node(Box<DisplayNode>),
    Nodes(Vec<DisplayNode>),
}

impl DisplayBlock {
    #[must_use]
    pub fn nodes(&self) -> &[DisplayNode] {
        match self {
            Self::Node(node) => std::slice::from_ref(node),
            Self::Nodes(nodes) => nodes,
        }
    }
}

/// One node in a display block's layout tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayNode {
    /// A responsive column grid; children place themselves with `span`.
    Grid {
        columns: Responsive,
        #[serde(default)]
        gap: Option<Gap>,
        #[serde(default)]
        span: Option<Responsive>,
        children: Vec<DisplayNode>,
    },
    /// A vertical flow of children.
    Stack {
        #[serde(default)]
        gap: Option<Gap>,
        #[serde(default)]
        span: Option<Responsive>,
        children: Vec<DisplayNode>,
    },
    /// A boxed panel.
    Card {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        padding: Option<SpaceToken>,
        #[serde(default)]
        span: Option<Responsive>,
        children: Vec<DisplayNode>,
    },
    /// An unboxed group under a heading.
    Section {
        title: String,
        #[serde(default)]
        span: Option<Responsive>,
        children: Vec<DisplayNode>,
    },
    Divider {
        #[serde(default)]
        span: Option<Responsive>,
    },
    /// One record value (or several, for `badges`), with a declarative format.
    Field {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        fields: Vec<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        empty: Option<String>,
        #[serde(default)]
        format: Option<FieldFormat>,
        #[serde(default)]
        template: Option<String>,
        #[serde(default)]
        actions: Vec<FieldAction>,
        #[serde(default)]
        span: Option<Responsive>,
    },
    /// Records of another model that reference this record.
    Resource {
        model: String,
        /// The reference field on that model; required only when it has
        /// several references to this model.
        #[serde(default)]
        via: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        display: ResourceDisplay,
        /// For `display: item` — a block name on the related model.
        #[serde(default)]
        item: Option<String>,
        /// For `display: item` — grid columns for the item cards.
        #[serde(default)]
        columns: Option<Responsive>,
        /// For `display: table` — the columns.
        #[serde(default)]
        fields: Vec<TableField>,
        /// For `display: table` or `tree` — display nodes rendered in the
        /// related record's own context inside a per-row expand/contract
        /// disclosure (e.g. a document's Markdown body).
        #[serde(default)]
        expand: Vec<DisplayNode>,
        /// For `display: tree` — the self-reference field on the related
        /// model that links a record to its parent record.
        #[serde(default)]
        tree: Option<String>,
        /// For `display: checklist` — the boolean field toggled in place.
        #[serde(default)]
        check: Option<String>,
        #[serde(default)]
        order: Vec<Order>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        empty: Option<String>,
        #[serde(default)]
        actions: Vec<ResourceAction>,
        #[serde(default)]
        span: Option<Responsive>,
    },
    /// The record's generated artifacts (model outputs).
    Outputs {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        display: OutputsDisplay,
        #[serde(default)]
        span: Option<Responsive>,
    },
    /// Reserved for the actions pass; parses so projects can stage config,
    /// but validation rejects it as not yet supported.
    ActionGroup {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        appearance: Option<String>,
        #[serde(default)]
        actions: Vec<Value>,
        #[serde(default)]
        span: Option<Responsive>,
    },
}

impl DisplayNode {
    fn children(&self) -> &[DisplayNode] {
        match self {
            Self::Grid { children, .. }
            | Self::Stack { children, .. }
            | Self::Card { children, .. }
            | Self::Section { children, .. } => children,
            _ => &[],
        }
    }

    fn span(&self) -> Option<&Responsive> {
        match self {
            Self::Grid { span, .. }
            | Self::Stack { span, .. }
            | Self::Card { span, .. }
            | Self::Section { span, .. }
            | Self::Divider { span }
            | Self::Field { span, .. }
            | Self::Resource { span, .. }
            | Self::Outputs { span, .. }
            | Self::ActionGroup { span, .. } => span.as_ref(),
        }
    }
}

/// A value that is either fixed or varies by breakpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Responsive {
    Fixed(u32),
    Breakpoints(ResponsiveMap),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsiveMap {
    #[serde(default)]
    pub default: Option<u32>,
    #[serde(default)]
    pub sm: Option<u32>,
    #[serde(default)]
    pub md: Option<u32>,
    #[serde(default)]
    pub lg: Option<u32>,
    #[serde(default)]
    pub xl: Option<u32>,
}

impl Responsive {
    /// The value in effect at each breakpoint, cascading upward from
    /// `default` (an unset breakpoint inherits the nearest smaller one).
    #[must_use]
    pub fn cascade(&self, fallback: u32) -> [u32; 5] {
        match self {
            Self::Fixed(value) => [*value; 5],
            Self::Breakpoints(map) => {
                let steps = [map.default, map.sm, map.md, map.lg, map.xl];
                let mut current = map.default.unwrap_or(fallback);
                steps.map(|step| {
                    current = step.unwrap_or(current);
                    current
                })
            }
        }
    }

    fn values(&self) -> Vec<u32> {
        match self {
            Self::Fixed(value) => vec![*value],
            Self::Breakpoints(map) => [map.default, map.sm, map.md, map.lg, map.xl]
                .into_iter()
                .flatten()
                .collect(),
        }
    }
}

/// Spacing between or inside layout nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Gap {
    Uniform(SpaceToken),
    Axes {
        #[serde(default)]
        column: Option<SpaceToken>,
        #[serde(default)]
        row: Option<SpaceToken>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceToken {
    Sm,
    Md,
    Lg,
    Xl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldFormat {
    Text,
    Title,
    Markdown,
    Code,
    Date,
    RelativeTime,
    Badge,
    Badges,
    Chips,
    List,
    Links,
    Template,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldAction {
    /// Copy the value to the clipboard.
    Copy {
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        value: CopyValue,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyValue {
    #[default]
    Text,
    /// Markdown rendered to HTML.
    Html,
    /// Array items joined with newlines.
    Lines,
    Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceDisplay {
    /// Each record through a named block (else its `card` block, else the
    /// built-in card).
    #[default]
    Item,
    Table,
    /// A table whose rows nest by a self-reference on the related model
    /// (`tree:` names that field); columns and expand work as for `table`.
    Tree,
    Checklist,
    Summary,
}

/// A table column: a bare field name or a spec with format/label overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TableField {
    Name(String),
    Spec {
        field: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        format: Option<FieldFormat>,
    },
}

impl TableField {
    #[must_use]
    pub fn field(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Spec { field, .. } => field,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceAction {
    /// Open the related model's create form with the reference prefilled.
    Create {
        #[serde(default)]
        label: Option<String>,
    },
    /// Link to a view. With `filtered`, the link pins the view to this
    /// record: an equality filter on the related model's back-reference.
    Navigate {
        #[serde(default)]
        label: Option<String>,
        view: String,
        #[serde(default)]
        filtered: bool,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputsDisplay {
    #[default]
    Table,
    List,
}

/// Display configuration on a view: which block renders each record in list
/// contexts (default: the model's `card` block, else the built-in card).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewDisplay {
    #[serde(default)]
    pub item: Option<String>,
}

/// Pseudo-fields addressable alongside model fields in `field` nodes.
const META_FIELDS: [&str; 3] = ["meta.key", "meta.path", "meta.model"];

fn valid_block_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Intra-model checks for every display block: known fields, sane layout
/// numbers, coherent field-node configuration. Called from `validate_model`.
#[must_use]
pub fn validate_model_display(model: &Model) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, block) in &model.display {
        let location = format!("model {}.display.{name}", model.name);
        if !valid_block_name(name) {
            problems.push(Problem::new(
                &location,
                "block names must be lowercase [a-z0-9_]+",
            ));
        }
        for (index, node) in block.nodes().iter().enumerate() {
            let path = if matches!(block, DisplayBlock::Node(_)) {
                location.clone()
            } else {
                format!("{location}[{index}]")
            };
            validate_node(model, node, None, &path, &mut problems);
        }
    }
    problems
}

fn validate_node(
    model: &Model,
    node: &DisplayNode,
    parent_columns: Option<&Responsive>,
    path: &str,
    problems: &mut Vec<Problem>,
) {
    if let Some(span) = node.span() {
        match parent_columns {
            None => problems.push(Problem::new(
                path,
                "span is only valid directly inside a grid",
            )),
            Some(columns) => {
                let spans = span.cascade(1);
                let cols = columns.cascade(1);
                if span.values().is_empty() {
                    problems.push(Problem::new(path, "span must set at least one breakpoint"));
                }
                for ((name, _), (s, c)) in BREAKPOINTS.iter().zip(spans.iter().zip(cols.iter())) {
                    if *s == 0 || s > c {
                        problems.push(Problem::new(
                            path,
                            format!("span {s} does not fit {c} columns (at {name})"),
                        ));
                        break;
                    }
                }
            }
        }
    }
    let child_columns = match node {
        DisplayNode::Grid { columns, .. } => {
            let values = columns.values();
            if values.is_empty() {
                problems.push(Problem::new(
                    path,
                    "columns must set at least one breakpoint",
                ));
            }
            for value in values {
                if !(1..=12).contains(&value) {
                    problems.push(Problem::new(path, "columns must be between 1 and 12"));
                    break;
                }
            }
            Some(columns)
        }
        _ => None,
    };
    match node {
        DisplayNode::Field {
            name,
            fields,
            format,
            template,
            ..
        } => {
            let multi = matches!(format, Some(FieldFormat::Badge | FieldFormat::Badges));
            match (name, fields.is_empty()) {
                (Some(_), false) => {
                    problems.push(Problem::new(path, "set either name or fields, not both"));
                }
                (None, true) => problems.push(Problem::new(path, "field nodes require a name")),
                (None, false) if !multi => problems.push(Problem::new(
                    path,
                    "fields (plural) requires format badge or badges",
                )),
                _ => {}
            }
            if let Some(name) = name {
                check_display_field(model, name, path, problems);
            }
            for field in fields {
                check_display_field(model, field, path, problems);
            }
            if matches!(format, Some(FieldFormat::Template)) != template.is_some() {
                problems.push(Problem::new(
                    path,
                    "template format and a template string go together",
                ));
            }
        }
        DisplayNode::Resource {
            display,
            item,
            check,
            expand,
            tree,
            ..
        } => {
            // Cross-model details are checked in validate_display_references;
            // here only the shape of this node.
            if matches!(display, ResourceDisplay::Checklist) && check.is_none() {
                problems.push(Problem::new(
                    path,
                    "checklist requires check: <boolean field>",
                ));
            }
            if !matches!(display, ResourceDisplay::Checklist) && check.is_some() {
                problems.push(Problem::new(path, "check is only valid for checklist"));
            }
            if !matches!(display, ResourceDisplay::Item) && item.is_some() {
                problems.push(Problem::new(path, "item is only valid for display: item"));
            }
            if !matches!(display, ResourceDisplay::Table | ResourceDisplay::Tree)
                && !expand.is_empty()
            {
                problems.push(Problem::new(
                    path,
                    "expand is only valid for display: table or tree",
                ));
            }
            if matches!(display, ResourceDisplay::Tree) && tree.is_none() {
                problems.push(Problem::new(
                    path,
                    "tree display requires tree: <self-reference field>",
                ));
            }
            if !matches!(display, ResourceDisplay::Tree) && tree.is_some() {
                problems.push(Problem::new(path, "tree is only valid for display: tree"));
            }
        }
        DisplayNode::Outputs { .. } if model.outputs.is_empty() => {
            problems.push(Problem::new(path, "model has no outputs"));
        }
        DisplayNode::ActionGroup { .. } => {
            problems.push(Problem::new(
                path,
                "action_group is not yet supported (reserved for the actions pass)",
            ));
        }
        _ => {}
    }
    for (index, child) in node.children().iter().enumerate() {
        let child_path = format!("{path}.children[{index}]");
        validate_node(model, child, child_columns, &child_path, problems);
    }
}

fn check_display_field(model: &Model, name: &str, path: &str, problems: &mut Vec<Problem>) {
    if !model.fields.contains_key(name) && !META_FIELDS.contains(&name) {
        problems.push(Problem::new(
            path,
            format!("unknown field {name:?} on model {}", model.name),
        ));
    }
}

/// Cross-definition checks: resource targets, item blocks, table columns, and
/// navigate views. Requires every model and view, so it runs during workspace
/// validation rather than per-file.
#[must_use]
pub fn validate_display_references(
    model: &Model,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (name, block) in &model.display {
        let location = format!("model {}.display.{name}", model.name);
        for (index, node) in block.nodes().iter().enumerate() {
            let path = if matches!(block, DisplayBlock::Node(_)) {
                location.clone()
            } else {
                format!("{location}[{index}]")
            };
            check_node_references(model, node, models, views, &path, &mut problems);
        }
    }
    problems
}

fn check_node_references(
    model: &Model,
    node: &DisplayNode,
    models: &BTreeMap<String, Model>,
    views: &BTreeMap<String, View>,
    path: &str,
    problems: &mut Vec<Problem>,
) {
    if let DisplayNode::Resource {
        model: related_name,
        via,
        item,
        fields,
        check,
        order,
        actions,
        expand,
        tree,
        ..
    } = node
    {
        let Some(related) = models.get(related_name) else {
            problems.push(Problem::new(
                path,
                format!("unknown model {related_name:?}"),
            ));
            return;
        };
        let references: Vec<&String> = related
            .fields
            .iter()
            .filter(|(_, field)| {
                field
                    .reference
                    .as_ref()
                    .is_some_and(|reference| reference.model == model.name)
            })
            .map(|(name, _)| name)
            .collect();
        match via {
            Some(via) if !references.contains(&via) => {
                problems.push(Problem::new(
                    path,
                    format!(
                        "field {via:?} on {related_name} does not reference {}",
                        model.name
                    ),
                ));
            }
            None if references.is_empty() => problems.push(Problem::new(
                path,
                format!("{related_name} has no reference to {}", model.name),
            )),
            None if references.len() > 1 => problems.push(Problem::new(
                path,
                format!(
                    "{related_name} references {} through several fields ({}); set via",
                    model.name,
                    references
                        .iter()
                        .map(|name| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
            _ => {}
        }
        if let Some(item) = item
            && !related.display.contains_key(item)
        {
            problems.push(Problem::new(
                path,
                format!("{related_name} has no display block {item:?}"),
            ));
        }
        if let Some(check) = check
            && related
                .fields
                .get(check)
                .is_none_or(|field| field.field_type != FieldType::Boolean)
        {
            problems.push(Problem::new(
                path,
                format!("check field {check:?} must be a boolean on {related_name}"),
            ));
        }
        for field in fields {
            if !related.fields.contains_key(field.field()) && !META_FIELDS.contains(&field.field())
            {
                problems.push(Problem::new(
                    path,
                    format!("unknown field {:?} on model {related_name}", field.field()),
                ));
            }
        }
        for order in order {
            if !related.fields.contains_key(&order.field) {
                problems.push(Problem::new(
                    path,
                    format!(
                        "unknown order field {:?} on model {related_name}",
                        order.field
                    ),
                ));
            }
        }
        for action in actions {
            if let ResourceAction::Navigate { view, filtered, .. } = action {
                match views.get(view) {
                    None => {
                        problems.push(Problem::new(path, format!("unknown view {view:?}")));
                    }
                    Some(target) if *filtered && target.model != *related_name => {
                        problems.push(Problem::new(
                            path,
                            format!(
                                "filtered navigate must target a view of {related_name}, but {view:?} shows {}",
                                target.model
                            ),
                        ));
                    }
                    _ => {}
                }
            }
        }
        if let Some(tree) = tree {
            let is_self_reference = related.fields.get(tree).is_some_and(|field| {
                field
                    .reference
                    .as_ref()
                    .is_some_and(|reference| reference.model == *related_name)
            });
            if !is_self_reference {
                problems.push(Problem::new(
                    path,
                    format!("tree field {tree:?} must be a self-reference on {related_name}"),
                ));
            }
        }
        // Expand nodes render in the related record's context, so both their
        // shape and their references validate against the related model.
        for (index, expand_node) in expand.iter().enumerate() {
            let expand_path = format!("{path}.expand[{index}]");
            validate_node(related, expand_node, None, &expand_path, problems);
            check_node_references(related, expand_node, models, views, &expand_path, problems);
        }
    }
    for (index, child) in node.children().iter().enumerate() {
        let child_path = format!("{path}.children[{index}]");
        check_node_references(model, child, models, views, &child_path, problems);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate_model;

    fn model(yaml: &str) -> Model {
        serde_yaml::from_str(yaml).expect("model fixture should parse")
    }

    const BOOK: &str = r#"
version: 1
name: Book
storage: { kind: directory, path: "books/{slug}" }
fields:
  slug: { type: string, source: { kind: path, variable: slug } }
  title: { type: string, source: { kind: yaml, file: book.yml, key: title } }
  state:
    type: enum
    source: { kind: yaml, file: book.yml, key: state }
    validation: { choices: [draft, done] }
outputs:
  epub: "books/{slug}/build/{slug}.epub"
"#;

    const TODO: &str = r#"
version: 1
name: Todo
storage: { kind: file, path: "todos/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
  title: { type: string, source: { kind: frontmatter, key: title } }
  done: { type: boolean, source: { kind: frontmatter, key: done } }
  book:
    type: reference
    source: { kind: frontmatter, key: book }
    reference: { model: Book, field: slug }
"#;

    fn book_with_display(display: &str) -> Model {
        model(&format!("{BOOK}display:\n{display}"))
    }

    fn display_problems(display: &str) -> Vec<Problem> {
        validate_model_display(&book_with_display(display))
    }

    #[test]
    fn accepts_a_full_layout_tree() {
        let book = book_with_display(
            r"
  detail:
    - type: grid
      columns: { default: 1, lg: 12 }
      gap: lg
      children:
        - type: stack
          span: { lg: 8 }
          children:
            - type: card
              children:
                - type: grid
                  columns: 2
                  gap: { column: xl, row: lg }
                  children:
                    - { type: field, name: title }
                    - { type: field, name: meta.path, format: code }
                - type: divider
                - { type: field, name: state, format: badge, empty: None }
        - type: section
          title: Extras
          span: { lg: 4 }
          children:
            - { type: outputs, display: table }
  compact:
    type: card
    children:
      - { type: field, name: title, format: title }
",
        );
        assert_eq!(validate_model(&book), Vec::new());
    }

    #[test]
    fn rejects_bad_field_nodes() {
        for (display, expected) in [
            (
                "  detail:\n    - { type: field, name: missing }",
                "unknown field",
            ),
            ("  detail:\n    - { type: field }", "require a name"),
            (
                "  detail:\n    - { type: field, name: title, fields: [state] }",
                "not both",
            ),
            (
                "  detail:\n    - { type: field, fields: [state, title] }",
                "format badge or badges",
            ),
            (
                "  detail:\n    - { type: field, name: title, format: template }",
                "template",
            ),
            (
                "  detail:\n    - { type: field, name: title, span: 2 }",
                "inside a grid",
            ),
        ] {
            let problems = display_problems(display);
            assert!(
                problems.iter().any(|p| p.message.contains(expected)),
                "expected {expected:?} for {display}, got {problems:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_layout_numbers() {
        let problems =
            display_problems("  detail:\n    - type: grid\n      columns: 13\n      children: []");
        assert!(problems[0].message.contains("between 1 and 12"));

        let problems = display_problems(
            "  detail:\n    - type: grid\n      columns: { default: 2, lg: 4 }\n      children:\n        - { type: divider, span: 3 }",
        );
        assert!(
            problems[0]
                .message
                .contains("span 3 does not fit 2 columns (at default)"),
            "{problems:?}"
        );
    }

    #[test]
    fn locates_problems_by_tree_path() {
        let problems = display_problems(
            "  detail:\n    - type: stack\n      children:\n        - { type: field, name: missing }",
        );
        assert_eq!(
            problems[0].location,
            "model Book.display.detail[0].children[0]"
        );
    }

    #[test]
    fn rejects_action_groups_for_now() {
        let problems = display_problems("  detail:\n    - { type: action_group, title: Builds }");
        assert!(problems[0].message.contains("not yet supported"));
    }

    #[test]
    fn checks_checklist_and_outputs_shapes() {
        let problems = display_problems(
            "  detail:\n    - { type: resource, model: Todo, display: checklist }",
        );
        assert!(problems[0].message.contains("checklist requires check"));

        let mut book = book_with_display("  detail:\n    - { type: outputs }");
        book.outputs.clear();
        let problems = validate_model_display(&book);
        assert!(problems[0].message.contains("no outputs"));
    }

    #[test]
    fn checks_resource_references_across_models() {
        let todo = model(TODO);
        let views = BTreeMap::new();

        let ok = book_with_display(
            "  detail:\n    - { type: resource, model: Todo, display: checklist, check: done, order: [{field: title}], fields: [title, done] }",
        );
        let models = BTreeMap::from([("Book".to_owned(), ok.clone()), ("Todo".to_owned(), todo)]);
        assert_eq!(
            validate_display_references(&ok, &models, &views),
            Vec::new()
        );

        for (display, expected) in [
            (
                "  detail:\n    - { type: resource, model: Nope }",
                "unknown model",
            ),
            (
                "  detail:\n    - { type: resource, model: Todo, via: title }",
                "does not reference",
            ),
            (
                "  detail:\n    - { type: resource, model: Todo, item: missing }",
                "no display block",
            ),
            (
                "  detail:\n    - { type: resource, model: Todo, display: checklist, check: title }",
                "must be a boolean",
            ),
            (
                "  detail:\n    - { type: resource, model: Todo, display: table, fields: [missing] }",
                "unknown field",
            ),
            (
                "  detail:\n    - { type: resource, model: Todo, actions: [{type: navigate, view: nope}] }",
                "unknown view",
            ),
        ] {
            let book = book_with_display(display);
            let models = BTreeMap::from([
                ("Book".to_owned(), book.clone()),
                ("Todo".to_owned(), model(TODO)),
            ]);
            let problems = validate_display_references(&book, &models, &views);
            assert!(
                problems.iter().any(|p| p.message.contains(expected)),
                "expected {expected:?} for {display}, got {problems:?}"
            );
        }
    }

    #[test]
    fn expand_validates_against_the_related_model() {
        let views = BTreeMap::new();

        // Expand nodes are the related model's namespace: title is a Todo
        // field (fine), state is a Book field (unknown over there).
        let ok = book_with_display(
            "  detail:\n    - { type: resource, model: Todo, display: table, fields: [title], expand: [{ type: field, name: title }] }",
        );
        let models = BTreeMap::from([
            ("Book".to_owned(), ok.clone()),
            ("Todo".to_owned(), model(TODO)),
        ]);
        assert_eq!(
            validate_display_references(&ok, &models, &views),
            Vec::new()
        );

        let wrong_model = book_with_display(
            "  detail:\n    - { type: resource, model: Todo, display: table, fields: [title], expand: [{ type: field, name: state }] }",
        );
        let models = BTreeMap::from([
            ("Book".to_owned(), wrong_model.clone()),
            ("Todo".to_owned(), model(TODO)),
        ]);
        let problems = validate_display_references(&wrong_model, &models, &views);
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("unknown field") && p.message.contains("Todo")),
            "expected an unknown-field problem on Todo, got {problems:?}"
        );

        let not_table = display_problems(
            "  detail:\n    - { type: resource, model: Todo, display: summary, expand: [{ type: field, name: title }] }",
        );
        assert!(
            not_table
                .iter()
                .any(|p| p.message.contains("only valid for display: table")),
            "expected a display-shape problem, got {not_table:?}"
        );
    }

    #[test]
    fn resource_without_reference_needs_none_and_ambiguity_needs_via() {
        let unrelated = model(
            r#"
version: 1
name: Note
storage: { kind: file, path: "notes/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
  body: { type: text, source: { kind: markdown } }
"#,
        );
        let book = book_with_display("  detail:\n    - { type: resource, model: Note }");
        let models = BTreeMap::from([
            ("Book".to_owned(), book.clone()),
            ("Note".to_owned(), unrelated),
        ]);
        let problems = validate_display_references(&book, &models, &BTreeMap::new());
        assert!(problems[0].message.contains("no reference to Book"));

        let twice = model(
            r#"
version: 1
name: Link
storage: { kind: file, path: "links/{id}.md" }
fields:
  id: { type: string, source: { kind: path, variable: id } }
  from:
    type: reference
    source: { kind: frontmatter, key: from }
    reference: { model: Book, field: slug }
  to:
    type: reference
    source: { kind: frontmatter, key: to }
    reference: { model: Book, field: slug }
"#,
        );
        let book = book_with_display("  detail:\n    - { type: resource, model: Link }");
        let models = BTreeMap::from([
            ("Book".to_owned(), book.clone()),
            ("Link".to_owned(), twice),
        ]);
        let problems = validate_display_references(&book, &models, &BTreeMap::new());
        assert!(problems[0].message.contains("set via"), "{problems:?}");
    }

    #[test]
    fn responsive_values_cascade_upward() {
        let map: Responsive = serde_yaml::from_str("{ default: 1, md: 2, xl: 3 }").unwrap();
        assert_eq!(map.cascade(1), [1, 1, 2, 2, 3]);
        let sparse: Responsive = serde_yaml::from_str("{ lg: 8 }").unwrap();
        assert_eq!(sparse.cascade(1), [1, 1, 1, 8, 8]);
        assert_eq!(Responsive::Fixed(4).cascade(1), [4; 5]);
    }
}

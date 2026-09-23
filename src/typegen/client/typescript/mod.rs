//! TypeScript `ClientPrinter` — interfaces for gRPC client wrappers. The
//! [`printer`] renders each construct (documents, their `…Data` inputs,
//! sub-types, the slug union); [`types`] maps one field to its property.

mod printer;
mod types;

pub(super) use printer::TsPrinter;

#[cfg(test)]
mod test_helpers {
    use crate::{
        core::{
            CollectionDefinition, FieldAdmin, FieldDefinition, FieldType, GlobalDefinition,
            LocalizedString, Registry, SelectOption,
        },
        typegen::client::drive,
    };

    use super::TsPrinter;

    /// One generated `export interface` block, from its header to its closing brace.
    pub fn interface_block<'a>(out: &'a str, header: &str) -> &'a str {
        let start = out
            .find(header)
            .unwrap_or_else(|| panic!("{header} not emitted:\n{out}"));
        let rest = &out[start..];

        &rest[..rest.find("\n}").map_or(rest.len(), |i| i + 2)]
    }

    /// A code field with a language allow-list.
    pub fn code_with_languages(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["python".to_string()])
                    .build(),
            )
            .build()
    }

    pub fn render(registry: &Registry) -> String {
        drive(registry, Box::new(TsPrinter::new()))
    }

    pub fn render_collection(out: &mut String, col: &CollectionDefinition) {
        let mut r = Registry::new();
        r.register_collection(col.clone());
        out.push_str(&render(&r));
    }

    pub fn render_global(out: &mut String, global: &GlobalDefinition) {
        let mut r = Registry::new();
        r.register_global(global.clone());
        out.push_str(&render(&r));
    }

    pub fn text_field(name: &str, required: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(required)
            .build()
    }

    pub fn select_field(name: &str, opts: &[&str]) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Select)
            .required(true)
            .options(
                opts.iter()
                    .map(|v| SelectOption::new(LocalizedString::Plain(v.to_string()), *v))
                    .collect(),
            )
            .build()
    }

    pub fn make_col(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.timestamps = true;
        def.fields = fields;
        def
    }
}

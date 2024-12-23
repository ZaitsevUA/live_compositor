use crate::definition::{Kind, TypeDefinition};

use super::MarkdownGenerator;

impl MarkdownGenerator<'_> {
    pub(super) fn write_type_definition(&mut self, def: TypeDefinition) {
        if let Some(name) = &def.name {
            if let Some(override_fn) = self.config.overrides.get(name.as_str()) {
                override_fn(self, def);
                return;
            }
        }

        match def.kind {
            Kind::Null => self.add_text("null"),
            Kind::I32 => self.add_text("i32"),
            Kind::F32 => self.add_text("f32"),
            Kind::F64 => self.add_text("f64"),
            Kind::U32 => self.add_text("u32"),
            Kind::U16 => self.add_text("u16"),
            Kind::U8 => self.add_text("u8"),
            Kind::Bool => self.add_text("bool"),
            Kind::Ref(reference) => self.add_text(reference),
            Kind::String { specific_value } => self.write_string(specific_value),
            Kind::Tuple(types) => self.write_tuple(types),
            Kind::Union(variants) => self.write_union(variants),
            Kind::Array { array_type } => self.write_array(*array_type),
            Kind::Object { fields } => self.write_object(fields),
            Kind::Map { value_type } => self.write_map(*value_type),
        }
    }

    fn write_string(&mut self, specific_value: Option<String>) {
        match specific_value {
            Some(value) => self.add_text(value),
            None => self.add_text("string"),
        }
    }

    fn write_tuple(&mut self, types: Vec<TypeDefinition>) {
        let types_len = types.len();

        self.add_text("[");
        for (i, def) in types.into_iter().enumerate() {
            self.write_type_definition(def);
            if i < types_len - 1 {
                self.add_text(", ");
            }
        }
        self.add_text("]");
    }

    fn write_union(&mut self, variants: Vec<TypeDefinition>) {
        let line_length = self.calculate_generation_length(|generator| {
            generator.write_single_line_union(variants.clone());
        });

        if line_length > 60 {
            self.write_multi_line_union(variants);
        } else {
            self.write_single_line_union(variants);
        }
    }

    fn write_single_line_union(&mut self, variants: Vec<TypeDefinition>) {
        let mut first = true;
        for variant in variants {
            if !first {
                self.add_text(" | ");
            }
            self.write_type_definition(variant);
            first = false;
        }
    }

    pub(super) fn write_multi_line_union(&mut self, variants: Vec<TypeDefinition>) {
        self.inc_indent();

        self.add_text("\n");

        let variants_len = variants.len();
        for (i, variant) in variants.into_iter().enumerate() {
            self.add_text("| ");

            self.inc_indent();
            self.write_type_definition(variant);
            self.dec_indent();

            if i < variants_len - 1 {
                self.add_text("\n");
            }
        }

        self.dec_indent();
    }

    fn write_array(&mut self, array_type: TypeDefinition) {
        self.write_type_definition(array_type);
        self.add_text("[]");
    }

    fn write_object(&mut self, fields: Vec<(String, TypeDefinition)>) {
        let line_length = self.calculate_generation_length(|generator| {
            generator.write_single_line_object(fields.clone());
        });

        if line_length > 50 {
            self.write_multi_line_object(fields);
        } else {
            self.write_single_line_object(fields);
        }
    }

    fn write_single_line_object(&mut self, fields: Vec<(String, TypeDefinition)>) {
        self.add_text("{ ");
        for (name, field_def) in fields {
            self.write_field_name(&name, field_def.is_optional);
            self.write_type_definition(field_def);
            self.add_text("; ");
        }
        self.add_text("}");
    }

    pub(super) fn write_multi_line_object(&mut self, fields: Vec<(String, TypeDefinition)>) {
        self.add_text("{\n");
        self.inc_indent();
        {
            for (name, field_def) in fields {
                self.write_field_name(&name, field_def.is_optional);
                self.write_type_definition(field_def);
                self.add_text(";\n");
            }
        }
        self.dec_indent();
        self.add_text("}");
    }

    fn write_map(&mut self, value_type: TypeDefinition) {
        self.add_text("Map<string, ");
        self.write_type_definition(value_type);
        self.add_text(">");
    }

    fn write_field_name(&mut self, name: &str, is_optional: bool) {
        match is_optional {
            true => self.add_text(format!("{name}?: ")),
            false => self.add_text(format!("{name}: ")),
        }
    }
}

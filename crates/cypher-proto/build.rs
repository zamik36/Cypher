use nom::{
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::{char, multispace0, space0, space1},
    combinator::value,
    multi::many0,
    sequence::preceded,
    IResult,
};
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone)]
struct Field {
    name: String,
    ty: FieldType,
}

#[derive(Debug, Clone)]
enum FieldType {
    Int,
    Long,
    Bytes,
    FString,
}

#[derive(Debug, Clone)]
struct Constructor {
    id: u32,
    method_name: String,
    fields: Vec<Field>,
    response_type: String,
}

fn is_hex_digit(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_method_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

fn parse_constructor_id(input: &str) -> IResult<&str, u32> {
    let (input, _) = tag("@0x")(input)?;
    let (input, hex_str) = take_while1(is_hex_digit)(input)?;
    let id = u32::from_str_radix(hex_str, 16).expect("valid hex");
    Ok((input, id))
}

fn parse_field_type(input: &str) -> IResult<&str, FieldType> {
    alt((
        value(FieldType::Int, tag("Int")),
        value(FieldType::Long, tag("Long")),
        value(FieldType::Bytes, tag("Bytes")),
        value(FieldType::FString, tag("String")),
    ))(input)
}

fn parse_field(input: &str) -> IResult<&str, Field> {
    let (input, name) = take_while1(is_ident_char)(input)?;
    let (input, _) = char(':')(input)?;
    let (input, ty) = parse_field_type(input)?;
    Ok((
        input,
        Field {
            name: name.to_string(),
            ty,
        },
    ))
}

fn parse_constructor(input: &str) -> IResult<&str, Constructor> {
    let (input, _) = multispace0(input)?;
    let (input, id) = parse_constructor_id(input)?;
    let (input, _) = space1(input)?;
    let (input, method_name) = take_while1(is_method_char)(input)?;
    let (input, fields) = many0(preceded(space1, parse_field))(input)?;
    let (input, _) = space0(input)?;
    let (input, _) = char('=')(input)?;
    let (input, _) = space0(input)?;
    let (input, response_type) = take_while1(is_method_char)(input)?;
    let (input, _) = char(';')(input)?;
    Ok((
        input,
        Constructor {
            id,
            method_name: method_name.to_string(),
            fields,
            response_type: response_type.to_string(),
        },
    ))
}

fn parse_line(input: &str) -> IResult<&str, Option<Constructor>> {
    let (input, _) = multispace0(input)?;
    if input.is_empty() {
        return Ok((input, None));
    }
    if input.starts_with("//") {
        let end = input.find('\n').unwrap_or(input.len());
        return Ok((&input[end..], None));
    }
    let (input, c) = parse_constructor(input)?;
    Ok((input, Some(c)))
}

fn parse_schema(mut input: &str) -> Vec<Constructor> {
    let mut constructors = Vec::new();
    while !input.trim().is_empty() {
        match parse_line(input) {
            Ok((rest, Some(c))) => {
                constructors.push(c);
                input = rest;
            }
            Ok((rest, None)) => {
                input = rest;
            }
            Err(e) => {
                panic!(
                    "Parse error: {:?}\nRemaining: {:?}",
                    e,
                    &input[..input.len().min(100)]
                );
            }
        }
    }
    constructors
}

fn to_struct_name(method_name: &str) -> String {
    method_name
        .split('.')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
            }
        })
        .collect()
}

fn rust_type(ty: &FieldType) -> &'static str {
    match ty {
        FieldType::Int => "u32",
        FieldType::Long => "u64",
        FieldType::Bytes => "Vec<u8>",
        FieldType::FString => "String",
    }
}

fn generate_serialize_field(field: &Field) -> String {
    match field.ty {
        FieldType::Int => {
            format!(
                "        buf.extend_from_slice(&self.{}.to_le_bytes());\n",
                field.name
            )
        }
        FieldType::Long => {
            format!(
                "        buf.extend_from_slice(&self.{}.to_le_bytes());\n",
                field.name
            )
        }
        FieldType::Bytes => {
            format!("        encode_bytes(&mut buf, &self.{});\n", field.name)
        }
        FieldType::FString => {
            format!("        encode_string(&mut buf, &self.{});\n", field.name)
        }
    }
}

fn generate_deserialize_field(field: &Field) -> String {
    let name = &field.name;
    // Use `__buf` as the raw bytes parameter name to avoid shadowing by field names like `data`
    match field.ty {
        FieldType::Int => {
            let mut s = String::new();
            s.push_str(&format!("        if offset + 4 > __buf.len() {{ return Err(cypher_common::Error::Protocol(\"truncated {}\".into())); }}\n", name));
            s.push_str(&format!("        let {} = u32::from_le_bytes(__buf[offset..offset+4].try_into().unwrap());\n", name));
            s.push_str("        offset += 4;\n");
            s
        }
        FieldType::Long => {
            let mut s = String::new();
            s.push_str(&format!("        if offset + 8 > __buf.len() {{ return Err(cypher_common::Error::Protocol(\"truncated {}\".into())); }}\n", name));
            s.push_str(&format!("        let {} = u64::from_le_bytes(__buf[offset..offset+8].try_into().unwrap());\n", name));
            s.push_str("        offset += 8;\n");
            s
        }
        FieldType::Bytes => {
            let mut s = String::new();
            s.push_str(&format!(
                "        let ({}, new_offset) = decode_bytes(__buf, offset)?;\n",
                name
            ));
            s.push_str("        offset = new_offset;\n");
            s
        }
        FieldType::FString => {
            let mut s = String::new();
            s.push_str(&format!(
                "        let ({}, new_offset) = decode_string(__buf, offset)?;\n",
                name
            ));
            s.push_str("        offset = new_offset;\n");
            s
        }
    }
}

fn generate_code(constructors: &[Constructor]) -> String {
    let mut code = String::new();

    for c in constructors {
        let struct_name = to_struct_name(&c.method_name);

        // Struct definition
        code.push_str(&format!(
            "/// Constructor `{}` (0x{:08X}) -> {}\n",
            c.method_name, c.id, c.response_type
        ));
        code.push_str("#[derive(Debug, Clone, PartialEq)]\n");
        code.push_str(&format!("pub struct {} {{\n", struct_name));
        for f in &c.fields {
            code.push_str(&format!("    pub {}: {},\n", f.name, rust_type(&f.ty)));
        }
        code.push_str("}\n\n");

        // Constructor ID constant
        code.push_str(&format!("impl {} {{\n", struct_name));
        code.push_str(&format!(
            "    pub const CONSTRUCTOR_ID: u32 = 0x{:08X};\n",
            c.id
        ));
        code.push_str("}\n\n");

        // Serializable impl - serialize
        code.push_str(&format!("impl Serializable for {} {{\n", struct_name));
        code.push_str("    fn serialize(&self) -> Vec<u8> {\n");
        code.push_str("        let mut buf = Vec::new();\n");
        code.push_str("        buf.extend_from_slice(&Self::CONSTRUCTOR_ID.to_le_bytes());\n");
        for f in &c.fields {
            code.push_str(&generate_serialize_field(f));
        }
        code.push_str("        buf\n");
        code.push_str("    }\n\n");

        // Serializable impl - deserialize
        // Note: use `__buf` as parameter name to avoid shadowing by field names (e.g. a field named `data`)
        code.push_str("    #[allow(unused_assignments)]\n");
        code.push_str("    fn deserialize(__buf: &[u8]) -> cypher_common::Result<Self> {\n");
        code.push_str("        if __buf.len() < 4 {\n");
        code.push_str(
            "            return Err(cypher_common::Error::Protocol(\"data too short\".into()));\n",
        );
        code.push_str("        }\n");
        code.push_str("        let cid = u32::from_le_bytes(__buf[0..4].try_into().unwrap());\n");
        code.push_str("        if cid != Self::CONSTRUCTOR_ID {\n");
        code.push_str("            return Err(cypher_common::Error::Protocol(format!(\"wrong constructor id: expected 0x{:08X}, got 0x{:08X}\", Self::CONSTRUCTOR_ID, cid)));\n");
        code.push_str("        }\n");
        if c.fields.is_empty() {
            code.push_str("        let _offset = 4usize;\n");
        } else {
            code.push_str("        let mut offset = 4usize;\n");
        }
        for f in &c.fields {
            code.push_str(&generate_deserialize_field(f));
        }
        code.push_str(&format!("        Ok({} {{\n", struct_name));
        for f in &c.fields {
            code.push_str(&format!("            {},\n", f.name));
        }
        code.push_str("        })\n");
        code.push_str("    }\n");
        code.push_str("}\n\n");
    }

    // Message enum
    code.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    code.push_str("pub enum Message {\n");
    for c in constructors {
        let struct_name = to_struct_name(&c.method_name);
        code.push_str(&format!("    {}({}),\n", struct_name, struct_name));
    }
    code.push_str("}\n\n");

    // dispatch function
    code.push_str(
        "/// Dispatch raw bytes to the correct Message variant by reading the constructor ID.\n",
    );
    code.push_str("pub fn dispatch(data: &[u8]) -> cypher_common::Result<Message> {\n");
    code.push_str("    if data.len() < 4 {\n");
    code.push_str("        return Err(cypher_common::Error::Protocol(\"data too short for dispatch\".into()));\n");
    code.push_str("    }\n");
    code.push_str("    let cid = u32::from_le_bytes(data[0..4].try_into().unwrap());\n");
    code.push_str("    match cid {\n");
    for c in constructors {
        let struct_name = to_struct_name(&c.method_name);
        code.push_str(&format!(
            "        {}::CONSTRUCTOR_ID => Ok(Message::{}({}::deserialize(data)?)),\n",
            struct_name, struct_name, struct_name
        ));
    }
    code.push_str("        _ => Err(cypher_common::Error::Protocol(format!(\"unknown constructor id: 0x{:08X}\", cid))),\n");
    code.push_str("    }\n");
    code.push_str("}\n\n");

    // Message::serialize
    code.push_str("impl Message {\n");
    code.push_str("    pub fn serialize(&self) -> Vec<u8> {\n");
    code.push_str("        match self {\n");
    for c in constructors {
        let struct_name = to_struct_name(&c.method_name);
        code.push_str(&format!(
            "            Message::{}(inner) => inner.serialize(),\n",
            struct_name
        ));
    }
    code.push_str("        }\n");
    code.push_str("    }\n");
    code.push_str("}\n");

    code
}

fn main() {
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("proto")
        .join("schema")
        .join("core.p2p");

    println!("cargo:rerun-if-changed={}", schema_path.display());

    let schema = fs::read_to_string(&schema_path).unwrap_or_else(|_e| String::new()); // schema may not exist during initial build

    if schema.is_empty() {
        // Write empty generated file
        let out_dir = env::var("OUT_DIR").unwrap();
        let dest = Path::new(&out_dir).join("proto_generated.rs");
        fs::write(&dest, "").unwrap();
        return;
    }

    let constructors = parse_schema(&schema);
    let code = generate_code(&constructors);

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("proto_generated.rs");
    let mut f = fs::File::create(&dest).unwrap();
    f.write_all(code.as_bytes()).unwrap();
}

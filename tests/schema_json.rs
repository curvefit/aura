use aura_codec::{
    canonicalize_schema_json, decode_schema_descriptor, encode_schema_descriptor,
    parse_schema_json, AuraError, DerivedExpression, DerivedExpressionOp, FieldRole, FieldType,
    GroupDescriptor, RelationshipPermissions, SchemaBuilder, SchemaDescriptor,
    SchemaEncodingVersion, MAX_SCHEMA_JSON_BYTES,
};
use serde_json::{json, Value};

const INPUT: &str = r#"
{
  "derived_expressions": [
    {
      "source": "external",
      "literals": [],
      "input_slots": [3],
      "op": "add_residual",
      "output_slot": 5,
      "id": 1
    }
  ],
  "groups": [
    {
      "relationship_permissions": ["joint_same_field", "split", "across_domain_same_field", "within_domain"],
      "dual_domain": {"domain_count": 2, "discriminator_slot": 2},
      "child_slots": [2, 3],
      "kind": "repeated",
      "id": 7
    },
    {
      "relationship_permissions": ["within_domain", "split"],
      "dual_domain": null,
      "child_slots": [4, 5],
      "kind": "repeated",
      "id": 3
    }
  ],
  "fields": [
    {
      "transform_candidates": ["bitpack", "delta_base", "absolute"],
      "relation": {"kind": "none"},
      "nullable": false,
      "scope": "repeated",
      "scale": 0,
      "role": "side",
      "type": "u8",
      "name": "side",
      "id": 2
    },
    {
      "id": 0,
      "name": "ts",
      "type": "timestamp_ns",
      "role": "timestamp",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["rough_step", "absolute", "fixed_step", "delta_previous", "delta_base"]
    },
    {
      "id": 1,
      "name": "reset",
      "type": "u8",
      "role": "flag",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["bitpack", "absolute", "delta_base"]
    },
    {
      "id": 5,
      "name": "residual",
      "type": "i64",
      "role": "value",
      "scale": -2,
      "scope": "repeated",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["bitpack", "zigzag_varint", "midpoint", "delta2", "delta_previous", "delta_base", "absolute"]
    },
    {
      "id": 3,
      "name": "price",
      "type": "i64",
      "role": "price",
      "scale": -2,
      "scope": "repeated",
      "nullable": false,
      "relation": {"kind": "none"},
      "transform_candidates": ["bitpack", "zigzag_varint", "midpoint", "delta2", "delta_previous", "delta_base", "absolute"]
    },
    {
      "id": 4,
      "name": "quantity",
      "type": "i64",
      "role": "quantity",
      "scale": 0,
      "scope": "repeated",
      "nullable": false,
      "relation": {"kind": "delta_from_field", "field_id": 3},
      "transform_candidates": ["bitpack", "zigzag_varint", "midpoint", "delta2", "delta_related", "delta_previous", "delta_base", "absolute"]
    }
  ],
  "name": "canonical_book",
  "schema_encoding": "v3",
  "schema_version": 1,
  "schema_format": "aura-schema"
}
"#;

fn input_value() -> Value {
    serde_json::from_str(INPUT).unwrap()
}

#[test]
fn canonical_flat_schema_matches_golden_json() {
    let input = r#"{
      "schema_format":"aura-schema",
      "schema_version":1,
      "schema_encoding":"v3",
      "name":"golden",
      "fields":[{
        "id":0,
        "name":"value",
        "type":"i64",
        "role":"value",
        "scale":0,
        "scope":"event",
        "nullable":false,
        "relation":{"kind":"none"},
        "transform_candidates":["absolute"]
      }],
      "groups":[],
      "derived_expressions":[]
    }"#;
    let canonical = canonicalize_schema_json(input).unwrap();
    let golden = r#"{
  "schema_format": "aura-schema",
  "schema_version": 1,
  "schema_encoding": "v3",
  "name": "golden",
  "schema_id": 106027077,
  "fields": [
    {
      "id": 0,
      "name": "value",
      "type": "i64",
      "role": "value",
      "scale": 0,
      "scope": "event",
      "nullable": false,
      "relation": {
        "kind": "none"
      },
      "transform_candidates": [
        "absolute"
      ]
    }
  ],
  "groups": [],
  "derived_expressions": []
}
"#;
    assert_eq!(golden, canonical);
}

#[test]
fn canonical_json_is_stable_and_has_one_trailing_newline() {
    let canonical = canonicalize_schema_json(INPUT).unwrap();
    assert!(canonical.starts_with("{\n  \"schema_format\": \"aura-schema\","));
    assert!(canonical.ends_with("}\n"));
    assert!(!canonical.ends_with("}\n\n"));
    assert_eq!(canonical, canonicalize_schema_json(&canonical).unwrap());
}

#[test]
fn formatting_key_and_input_array_order_do_not_change_canonical_identity() {
    let canonical = canonicalize_schema_json(INPUT).unwrap();
    let compact = serde_json::to_string(&input_value()).unwrap();
    assert_ne!(INPUT, compact);
    assert_eq!(canonical, canonicalize_schema_json(&compact).unwrap());

    let parsed = parse_schema_json(INPUT).unwrap();
    assert_eq!(SchemaEncodingVersion::V3, parsed.encoding_version);
    assert_eq!(
        vec![3, 7],
        parsed
            .groups
            .iter()
            .map(|group| group.group_id)
            .collect::<Vec<_>>()
    );
    assert!(canonical.find("\"id\": 3").unwrap() < canonical.rfind("\"id\": 7").unwrap());

    let mut reordered = input_value();
    reordered["fields"].as_array_mut().unwrap().reverse();
    reordered["groups"].as_array_mut().unwrap().reverse();
    let reordered_schema = parse_schema_json(&reordered.to_string()).unwrap();
    assert_eq!(parsed.schema_id, reordered_schema.schema_id);
    assert_eq!(canonical, reordered_schema.to_canonical_json().unwrap());
}

#[test]
fn semantic_child_partition_changes_the_schema_hash() {
    let first = parse_schema_json(INPUT).unwrap();
    let mut changed = input_value();
    changed["groups"][0]["child_slots"] = json!([2, 4]);
    changed["groups"][1]["child_slots"] = json!([3, 5]);
    let second = parse_schema_json(&changed.to_string()).unwrap();
    assert_ne!(first.schema_id, second.schema_id);
}

#[test]
fn semantic_child_field_order_changes_the_schema_hash() {
    let first = parse_schema_json(INPUT).unwrap();
    let mut changed = input_value();
    let fields = changed["fields"].as_array_mut().unwrap();
    let side = fields.iter_mut().find(|field| field["id"] == 2).unwrap();
    side["id"] = json!(3);
    let price = fields
        .iter_mut()
        .find(|field| field["name"] == "price")
        .unwrap();
    price["id"] = json!(2);
    let quantity = fields
        .iter_mut()
        .find(|field| field["name"] == "quantity")
        .unwrap();
    quantity["relation"]["field_id"] = json!(2);
    changed["derived_expressions"][0]["input_slots"] = json!([2]);
    let second = parse_schema_json(&changed.to_string()).unwrap();
    assert_ne!(first.schema_id, second.schema_id);
}

#[test]
fn derived_expression_input_order_is_canonicalized_by_id() {
    let mut value = input_value();
    value["derived_expressions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": 2,
            "output_slot": 3,
            "op": "add_residual",
            "input_slots": [0],
            "literals": [],
            "source": "internal"
        }));
    value["derived_expressions"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let schema = parse_schema_json(&value.to_string()).unwrap();
    assert_eq!(
        vec![1, 2],
        schema
            .derived_expressions
            .iter()
            .map(|expression| expression.expression_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn json_binary_descriptor_json_identity_is_exact() {
    let schema = SchemaDescriptor::from_json(INPUT).unwrap();
    let canonical = schema.to_canonical_json().unwrap();
    let binary = encode_schema_descriptor(&schema).unwrap();
    let decoded = decode_schema_descriptor(&binary).unwrap();
    assert_eq!(schema, decoded);
    assert_eq!(canonical, decoded.to_canonical_json().unwrap());
    assert_eq!(
        schema.schema_id,
        SchemaDescriptor::from_json(&canonical).unwrap().schema_id
    );
}

#[test]
fn exact_i128_opaque16_and_nullable_descriptor_fields_are_not_lossy() {
    let input = r#"{
      "schema_format":"aura-schema",
      "schema_version":1,
      "schema_encoding":"v3",
      "name":"wide_types",
      "fields":[
        {"id":0,"name":"wide","type":"i128","role":"value","scale":0,"scope":"event","nullable":false,"relation":{"kind":"none"},"transform_candidates":["absolute"]},
        {"id":1,"name":"opaque","type":"opaque16","role":"identifier","scale":0,"scope":"event","nullable":true,"relation":{"kind":"none"},"transform_candidates":["absolute"]}
      ],
      "groups":[],
      "derived_expressions":[]
    }"#;
    let schema = parse_schema_json(input).unwrap();
    assert_eq!(FieldType::I128, schema.fields[0].field_type);
    assert_eq!(FieldType::Opaque16, schema.fields[1].field_type);
    assert!(schema.fields[1].nullable);
    let binary = encode_schema_descriptor(&schema).unwrap();
    let decoded = decode_schema_descriptor(&binary).unwrap();
    assert_eq!(schema, decoded);
    assert_eq!(
        schema.to_canonical_json().unwrap(),
        parse_schema_json(&schema.to_canonical_json().unwrap())
            .unwrap()
            .to_canonical_json()
            .unwrap()
    );
}

#[test]
fn public_builder_and_json_produce_identical_hash_and_canonical_bytes() {
    let expressions =
        vec![DerivedExpression::new(1, 5, DerivedExpressionOp::AddResidual, vec![3]).unwrap()];
    let groups = vec![
        GroupDescriptor::dual_domain_repeated(
            7,
            vec![2, 3],
            2,
            RelationshipPermissions::none()
                .with_split()
                .with_within_domain()
                .with_across_domain_same_field()
                .with_joint_same_field(),
        ),
        GroupDescriptor::repeated(
            3,
            vec![4, 5],
            RelationshipPermissions::none()
                .with_split()
                .with_within_domain(),
        ),
    ];
    let built = SchemaBuilder::new("canonical_book")
        .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("reset", FieldType::U8, FieldRole::Flag)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .repeated_field_related_to("quantity", FieldType::I64, FieldRole::Quantity, "price")
        .repeated_field("residual", FieldType::I64, FieldRole::Value)
        .finish()
        .unwrap()
        .with_field_scales(vec![0, 0, 0, -2, 0, -2])
        .unwrap()
        .with_derived_expressions(expressions)
        .unwrap()
        .with_v3_groups(groups)
        .unwrap();
    let parsed = parse_schema_json(INPUT).unwrap();
    assert_eq!(built.schema_id, parsed.schema_id);
    assert_eq!(
        built.to_canonical_json().unwrap(),
        parsed.to_canonical_json().unwrap()
    );
}

#[test]
fn strict_json_rejects_unknown_keys_enums_duplicates_and_bad_numbers() {
    let mut unknown_root = input_value();
    unknown_root["metadata"] = json!({});
    assert!(parse_schema_json(&unknown_root.to_string()).is_err());

    let mut unknown_nested = input_value();
    unknown_nested["fields"][0]["venue"] = json!("none");
    assert!(parse_schema_json(&unknown_nested.to_string()).is_err());

    let mut bad_enum = input_value();
    bad_enum["fields"][0]["type"] = json!("I64");
    assert!(parse_schema_json(&bad_enum.to_string()).is_err());

    let mut float_id = input_value();
    float_id["fields"][0]["id"] = json!(2.5);
    assert!(parse_schema_json(&float_id.to_string()).is_err());

    let mut out_of_range = input_value();
    out_of_range["fields"][0]["id"] = json!(70000);
    assert!(parse_schema_json(&out_of_range.to_string()).is_err());

    let mut null_schema_id = input_value();
    null_schema_id["schema_id"] = Value::Null;
    assert!(parse_schema_json(&null_schema_id.to_string()).is_err());

    let duplicate_key = INPUT.replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1, \"schema_version\": 1,",
        1,
    );
    assert!(parse_schema_json(&duplicate_key).is_err());

    let duplicate_nested_key = INPUT.replacen(
        "\"nullable\": false,",
        "\"nullable\": false, \"nullable\": false,",
        1,
    );
    assert!(parse_schema_json(&duplicate_nested_key).is_err());

    for duplicate_at_level in [
        INPUT.replacen(
            "\"relation\": {\"kind\": \"none\"}",
            "\"relation\": {\"kind\": \"none\", \"kind\": \"none\"}",
            1,
        ),
        INPUT.replacen(
            "\"kind\": \"repeated\",\n      \"id\": 7",
            "\"kind\": \"repeated\", \"kind\": \"repeated\",\n      \"id\": 7",
            1,
        ),
        INPUT.replacen(
            "\"domain_count\": 2, \"discriminator_slot\": 2",
            "\"domain_count\": 2, \"domain_count\": 2, \"discriminator_slot\": 2",
            1,
        ),
        INPUT.replacen(
            "\"source\": \"external\",",
            "\"source\": \"external\", \"source\": \"external\",",
            1,
        ),
    ] {
        assert!(parse_schema_json(&duplicate_at_level).is_err());
    }
}

#[test]
fn strict_document_shape_rejects_bom_trailing_missing_null_and_excessive_nesting() {
    assert!(parse_schema_json(&format!("\u{feff}{INPUT}")).is_err());
    assert!(parse_schema_json(&format!("{INPUT} trailing")).is_err());

    for required in ["fields", "groups", "derived_expressions"] {
        let mut missing = input_value();
        missing.as_object_mut().unwrap().remove(required);
        assert!(parse_schema_json(&missing.to_string()).is_err());
    }

    let mut missing_dual_domain = input_value();
    missing_dual_domain["groups"][0]
        .as_object_mut()
        .unwrap()
        .remove("dual_domain");
    assert!(parse_schema_json(&missing_dual_domain.to_string()).is_err());

    let mut null_relation_id = input_value();
    null_relation_id["fields"][5]["relation"]["field_id"] = Value::Null;
    assert!(parse_schema_json(&null_relation_id.to_string()).is_err());

    let depth = 256;
    let deeply_nested = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
    assert!(deeply_nested.len() < MAX_SCHEMA_JSON_BYTES);
    assert!(parse_schema_json(&deeply_nested).is_err());
}

#[test]
fn semantic_validation_rejects_duplicate_non_dense_cycle_and_bad_id() {
    let mut duplicate_name = input_value();
    duplicate_name["fields"][1]["name"] = duplicate_name["fields"][0]["name"].clone();
    assert!(parse_schema_json(&duplicate_name.to_string()).is_err());

    let mut duplicate_id = input_value();
    duplicate_id["fields"][1]["id"] = duplicate_id["fields"][0]["id"].clone();
    assert!(parse_schema_json(&duplicate_id.to_string()).is_err());

    let mut non_dense = input_value();
    non_dense["fields"].as_array_mut().unwrap().remove(1);
    assert!(parse_schema_json(&non_dense.to_string()).is_err());

    let mut cycle = input_value();
    cycle["derived_expressions"][0]["input_slots"] = json!([3]);
    cycle["derived_expressions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": 2,
            "output_slot": 3,
            "op": "add_residual",
            "input_slots": [5],
            "literals": [],
            "source": "external"
        }));
    assert!(parse_schema_json(&cycle.to_string()).is_err());

    let mut bad_schema_id = input_value();
    bad_schema_id["schema_id"] = json!(1);
    assert_eq!(
        parse_schema_json(&bad_schema_id.to_string()),
        Err(AuraError::InvalidValue("schema id"))
    );

    let mut overlap = input_value();
    overlap["groups"][0]["child_slots"] = json!([2, 3, 4]);
    assert!(parse_schema_json(&overlap.to_string()).is_err());

    let mut bad_slot = input_value();
    bad_slot["groups"][0]["child_slots"] = json!([2, 99]);
    assert!(parse_schema_json(&bad_slot.to_string()).is_err());

    let mut bad_child_order = input_value();
    bad_child_order["groups"][0]["child_slots"] = json!([3, 2]);
    assert!(parse_schema_json(&bad_child_order.to_string()).is_err());

    let mut duplicate_candidate = input_value();
    duplicate_candidate["fields"][0]["transform_candidates"] = json!(["absolute", "absolute"]);
    assert!(parse_schema_json(&duplicate_candidate.to_string()).is_err());

    let mut duplicate_permission = input_value();
    duplicate_permission["groups"][0]["relationship_permissions"] = json!(["split", "split"]);
    assert!(parse_schema_json(&duplicate_permission.to_string()).is_err());

    let mut bad_relation = input_value();
    bad_relation["fields"][0]["relation"] = json!({"kind": "delta_from_field"});
    assert!(parse_schema_json(&bad_relation.to_string()).is_err());
}

#[test]
fn input_length_is_bounded_before_json_parse() {
    assert_eq!(16 * 1024 * 1024, MAX_SCHEMA_JSON_BYTES);
    let oversized = " ".repeat(MAX_SCHEMA_JSON_BYTES + 1);
    assert_eq!(
        parse_schema_json(&oversized),
        Err(AuraError::InvalidValue("schema json length"))
    );
}

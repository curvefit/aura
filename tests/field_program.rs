use aura_codec::program::{FieldCode, FieldProgram, ProgramOp, FIELD_AUX_EXTENDED};
use aura_codec::stats::PhysicalWidth;

#[test]
fn field_code_packs_operation_width_const_width_aux_and_flags() {
    let code = FieldCode::new(
        ProgramOp::DeltaRelated,
        PhysicalWidth::I16,
        PhysicalWidth::Zero,
        1,
        false,
        false,
    )
    .unwrap();

    assert_eq!(0b0000_1000_0100_0011, code.raw());

    let decoded = FieldCode::from_raw(code.raw()).unwrap();
    assert_eq!(ProgramOp::DeltaRelated, decoded.op().unwrap());
    assert_eq!(PhysicalWidth::I16, decoded.width().unwrap());
    assert_eq!(PhysicalWidth::Zero, decoded.const_width().unwrap());
    assert_eq!(1, decoded.aux());
    assert!(!decoded.has_base());
    assert!(!decoded.has_step());
}

#[test]
fn field_code_accepts_i128_fixed_width_code() {
    let code = FieldCode::new(
        ProgramOp::Absolute,
        PhysicalWidth::I128,
        PhysicalWidth::Zero,
        0,
        false,
        false,
    )
    .unwrap();

    assert_eq!(5, PhysicalWidth::I128.code());
    assert_eq!(16, PhysicalWidth::I128.byte_width());

    let decoded = FieldCode::from_raw(code.raw()).unwrap();
    assert_eq!(PhysicalWidth::I128, decoded.width().unwrap());
}

#[test]
fn field_program_round_trips_extended_refs_and_constants() {
    let program = FieldProgram {
        code: FieldCode::new(
            ProgramOp::DeltaRelated,
            PhysicalWidth::I32,
            PhysicalWidth::I64,
            FIELD_AUX_EXTENDED,
            true,
            false,
        )
        .unwrap(),
        reference_field_index: Some(12),
        base_value: Some(7_000_000_000),
        step: None,
    };

    let bytes = program.encode().unwrap();
    let decoded = FieldProgram::decode(&bytes).unwrap();

    assert_eq!(program, decoded);
}

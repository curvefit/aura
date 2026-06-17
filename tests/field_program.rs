use aura_codec::program::{FieldCode, FieldProgram, ProgramOp, FIELD_AUX_EXTENDED};
use aura_codec::stats::PhysicalWidth;
use aura_codec::FieldEncoding;
use aura_codec::PhysicalFieldPlan;

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
        bit_width: None,
    };

    let bytes = program.encode().unwrap();
    let decoded = FieldProgram::decode(&bytes).unwrap();

    assert_eq!(program, decoded);
}

#[test]
fn field_program_round_trips_bitpacked_delta_width() {
    let plan = PhysicalFieldPlan {
        field_index: 1,
        encoding: FieldEncoding::BitpackedDeltaPrevious,
        width: PhysicalWidth::Zero,
        bit_width: 15,
        reference_field_index: None,
        base_value: 6_354_071,
        step: 0,
        estimated_bytes: 0,
    };

    let program = FieldProgram::from_plan(plan).unwrap();
    let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

    assert_eq!(
        ProgramOp::BitpackedDeltaPrevious,
        decoded.code.op().unwrap()
    );
    assert_eq!(Some(15), decoded.bit_width);
    assert_eq!(plan, decoded.to_plan(1).unwrap());
}

#[test]
fn field_program_rejects_invalid_bit_width() {
    let plan = PhysicalFieldPlan {
        field_index: 1,
        encoding: FieldEncoding::BitpackedDeltaPrevious,
        width: PhysicalWidth::Zero,
        bit_width: 65,
        reference_field_index: None,
        base_value: 6_354_071,
        step: 0,
        estimated_bytes: 0,
    };

    assert_eq!(
        Err(aura_codec::AuraError::InvalidValue("bit width")),
        FieldProgram::from_plan(plan)
    );
}

#[test]
fn field_program_round_trips_derived_offset() {
    let plan = PhysicalFieldPlan {
        field_index: 6,
        encoding: FieldEncoding::DerivedOffset,
        width: PhysicalWidth::Zero,
        bit_width: 0,
        reference_field_index: Some(0),
        base_value: 59_999_000_000,
        step: 0,
        estimated_bytes: 0,
    };

    let program = FieldProgram::from_plan(plan).unwrap();
    let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

    assert_eq!(ProgramOp::DerivedOffset, decoded.code.op().unwrap());
    assert_eq!(Some(0), decoded.reference_field_index);
    assert_eq!(Some(59_999_000_000), decoded.base_value);
    assert_eq!(plan, decoded.to_plan(6).unwrap());
}

#[test]
fn field_program_round_trips_biased_bitpacked_related_delta() {
    let plan = PhysicalFieldPlan {
        field_index: 9,
        encoding: FieldEncoding::BitpackedDeltaRelatedOffset,
        width: PhysicalWidth::Zero,
        bit_width: 24,
        reference_field_index: Some(5),
        base_value: -9_330_228,
        step: 0,
        estimated_bytes: 0,
    };

    let program = FieldProgram::from_plan(plan).unwrap();
    let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

    assert_eq!(
        ProgramOp::BitpackedDeltaRelatedOffset,
        decoded.code.op().unwrap()
    );
    assert_eq!(Some(24), decoded.bit_width);
    assert_eq!(Some(-9_330_228), decoded.base_value);
    assert_eq!(plan, decoded.to_plan(9).unwrap());
}

#[test]
fn field_program_round_trips_biased_bitpacked_previous_delta() {
    let plan = PhysicalFieldPlan {
        field_index: 1,
        encoding: FieldEncoding::BitpackedDeltaPreviousOffset,
        width: PhysicalWidth::Zero,
        bit_width: 8,
        reference_field_index: None,
        base_value: 10_000,
        step: 1_000,
        estimated_bytes: 0,
    };

    let program = FieldProgram::from_plan(plan).unwrap();
    let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

    assert_eq!(
        ProgramOp::BitpackedDeltaPreviousOffset,
        decoded.code.op().unwrap()
    );
    assert_eq!(Some(8), decoded.bit_width);
    assert_eq!(Some(10_000), decoded.base_value);
    assert_eq!(Some(1_000), decoded.step);
    assert_eq!(plan, decoded.to_plan(1).unwrap());
}

#[test]
fn field_program_round_trips_max_min_residual_ops() {
    for (encoding, op) in [
        (
            FieldEncoding::BitpackedMaxPlusResidual,
            ProgramOp::BitpackedMaxPlusResidual,
        ),
        (
            FieldEncoding::BitpackedMinMinusResidual,
            ProgramOp::BitpackedMinMinusResidual,
        ),
    ] {
        let plan = PhysicalFieldPlan {
            field_index: 2,
            encoding,
            width: PhysicalWidth::Zero,
            bit_width: 13,
            reference_field_index: Some(1),
            base_value: 0,
            step: 4,
            estimated_bytes: 0,
        };

        let program = FieldProgram::from_plan(plan).unwrap();
        let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

        assert_eq!(op, decoded.code.op().unwrap());
        assert_eq!(Some(1), decoded.reference_field_index);
        assert_eq!(Some(4), decoded.step);
        assert_eq!(plan, decoded.to_plan(2).unwrap());
    }
}

#[test]
fn field_program_round_trips_product_and_proportional_residuals() {
    for (encoding, op) in [
        (
            FieldEncoding::BitpackedProductResidual,
            ProgramOp::BitpackedProductResidual,
        ),
        (
            FieldEncoding::BitpackedProportionalResidual,
            ProgramOp::BitpackedProportionalResidual,
        ),
    ] {
        let plan = PhysicalFieldPlan {
            field_index: 7,
            encoding,
            width: PhysicalWidth::Zero,
            bit_width: 36,
            reference_field_index: Some(5),
            base_value: -37_318_147_953,
            step: 1,
            estimated_bytes: 0,
        };

        let program = FieldProgram::from_plan(plan).unwrap();
        let decoded = FieldProgram::decode(&program.encode().unwrap()).unwrap();

        assert_eq!(op, decoded.code.op().unwrap());
        assert_eq!(Some(36), decoded.bit_width);
        assert_eq!(Some(-37_318_147_953), decoded.base_value);
        assert_eq!(plan, decoded.to_plan(7).unwrap());
    }
}

use aura_codec::bitpack::{
    pack_signed_values, pack_unsigned_values, unpack_signed_values, unpack_unsigned_values,
    unsigned_bitpack_width,
};
use aura_codec::AuraError;

#[test]
fn signed_bitpack_round_trips_negative_and_positive_values() {
    let values = [-3, -1, 0, 2, 3];

    let packed = pack_signed_values(&values, 3).unwrap();

    assert_eq!(2, packed.len());
    assert_eq!(
        values,
        unpack_signed_values(&packed, 3, values.len())
            .unwrap()
            .as_slice()
    );
}

#[test]
fn signed_bitpack_zero_width_only_accepts_zeroes() {
    let packed = pack_signed_values(&[0, 0, 0], 0).unwrap();

    assert!(packed.is_empty());
    assert_eq!(vec![0, 0, 0], unpack_signed_values(&packed, 0, 3).unwrap());
    assert_eq!(
        Err(AuraError::InvalidValue("bitpacked value")),
        pack_signed_values(&[0, 1], 0)
    );
}

#[test]
fn unsigned_bitpack_round_trips_biased_ranges() {
    let values = [0, 1, 7, 255, 511];
    let bit_width = unsigned_bitpack_width(511);

    let packed = pack_unsigned_values(&values, bit_width).unwrap();

    assert_eq!(6, packed.len());
    assert_eq!(
        values,
        unpack_unsigned_values(&packed, bit_width, values.len())
            .unwrap()
            .as_slice()
    );
}

#[test]
fn unsigned_bitpack_rejects_values_outside_width() {
    assert_eq!(
        Err(AuraError::InvalidValue("bitpacked value")),
        pack_unsigned_values(&[0, 4], 2)
    );
}

#[test]
fn unsigned_bitpack_round_trips_every_width() {
    for bit_width in 1..=64 {
        let upper = if bit_width == 64 {
            u64::MAX
        } else {
            (1u64 << bit_width) - 1
        };
        let values = [
            0,
            1.min(upper),
            upper / 3,
            upper / 2,
            upper.saturating_sub(1),
            upper,
        ];

        let packed = pack_unsigned_values(&values, bit_width).unwrap();

        assert_eq!(
            values,
            unpack_unsigned_values(&packed, bit_width, values.len())
                .unwrap()
                .as_slice(),
            "bit_width={bit_width}"
        );
    }
}

#[test]
fn signed_bitpack_round_trips_every_width() {
    for bit_width in 1..=64 {
        let (lower, upper) = if bit_width == 64 {
            (i64::MIN, i64::MAX)
        } else {
            let upper = (1i64 << (bit_width - 1)) - 1;
            let lower = -(1i64 << (bit_width - 1));
            (lower, upper)
        };
        let values = [
            0,
            upper,
            lower,
            upper / 2,
            lower / 2,
            upper.saturating_sub(1),
        ];

        let packed = pack_signed_values(&values, bit_width).unwrap();

        assert_eq!(
            values,
            unpack_signed_values(&packed, bit_width, values.len())
                .unwrap()
                .as_slice(),
            "bit_width={bit_width}"
        );
    }
}

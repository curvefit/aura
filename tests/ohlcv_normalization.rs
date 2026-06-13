use aura_codec::ohlcv;

#[test]
fn ohlcv_row_scales_seconds_prices_and_volume() {
    let row = ohlcv::ohlcv_i64_row(
        ohlcv::OhlcvF64 {
            ts_seconds: 1_780_272_000,
            open: 73_653.2,
            high: 73_659.6,
            low: 73_620.0,
            close: 73_659.5,
            volume: 48.353,
        },
        ohlcv::DecimalScales {
            price: 10,
            volume: 1_000,
        },
    )
    .unwrap();

    assert_eq!(
        vec![
            1_780_272_000_000_000_000,
            736_532,
            736_596,
            736_200,
            736_595,
            48_353,
        ],
        row
    );
}

#[test]
fn ohlcv_row_rejects_non_finite_values() {
    let err = ohlcv::ohlcv_i64_row(
        ohlcv::OhlcvF64 {
            ts_seconds: 1,
            open: f64::NAN,
            high: 2.0,
            low: 3.0,
            close: 4.0,
            volume: 5.0,
        },
        ohlcv::DecimalScales {
            price: 100,
            volume: 100,
        },
    )
    .unwrap_err();

    assert_eq!("invalid value for finite float", err.to_string());
}

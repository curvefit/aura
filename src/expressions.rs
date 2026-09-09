//! Exact derived-expression arithmetic shared by planning and replay.
//! Data access is a monomorphized closure; no row/column matrix or term Vec is built.
use crate::{AuraError, DerivedExpressionOp, Result};

pub(crate) fn evaluate(
    op: DerivedExpressionOp,
    input_slots: &[u16],
    literals: &[i64],
    value: impl Fn(u16) -> Result<i64>,
) -> Result<i64> {
    let input_terms = || input_slots.iter().map(|slot| value(*slot));
    match op {
        DerivedExpressionOp::Add => {
            let sum = input_terms()
                .chain(literals.iter().copied().map(Ok))
                .try_fold(0i128, |sum, term| {
                    sum.checked_add(i128::from(term?))
                        .ok_or(AuraError::InvalidValue("expression value"))
                })?;
            i64::try_from(sum).map_err(|_| AuraError::InvalidValue("expression value"))
        }
        DerivedExpressionOp::Sub | DerivedExpressionOp::Div => {
            let mut terms = input_terms().chain(literals.iter().copied().map(Ok));
            let first = terms
                .next()
                .transpose()?
                .ok_or(AuraError::InvalidValue("expression terms"))?;
            let value = terms.try_fold(i128::from(first), |value, term| {
                let term = i128::from(term?);
                match op {
                    DerivedExpressionOp::Sub => value
                        .checked_sub(term)
                        .ok_or(AuraError::InvalidValue("expression value")),
                    DerivedExpressionOp::Div if term != 0 => value
                        .checked_div(term)
                        .ok_or(AuraError::InvalidValue("expression value")),
                    _ => Err(AuraError::InvalidValue("expression value")),
                }
            })?;
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("expression value"))
        }
        DerivedExpressionOp::Mul => {
            let product = input_terms()
                .chain(literals.iter().copied().map(Ok))
                .try_fold(1i128, |product, term| {
                    product
                        .checked_mul(i128::from(term?))
                        .ok_or(AuraError::InvalidValue("expression value"))
                })?;
            i64::try_from(product).map_err(|_| AuraError::InvalidValue("expression value"))
        }
        DerivedExpressionOp::MulDiv => {
            let divisor = *literals
                .first()
                .filter(|divisor| **divisor != 0)
                .ok_or(AuraError::InvalidValue("expression terms"))?;
            let product = input_terms().try_fold(1i128, |product, term| {
                product
                    .checked_mul(i128::from(term?))
                    .ok_or(AuraError::InvalidValue("expression value"))
            })?;
            let value = product
                .checked_div(i128::from(divisor))
                .ok_or(AuraError::InvalidValue("expression value"))?;
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("expression value"))
        }
        DerivedExpressionOp::Min | DerivedExpressionOp::Max => input_terms()
            .chain(literals.iter().copied().map(Ok))
            .try_fold(None, |value, term| {
                let term = term?;
                Ok::<_, AuraError>(Some(value.map_or(term, |value: i64| match op {
                    DerivedExpressionOp::Min => value.min(term),
                    DerivedExpressionOp::Max => value.max(term),
                    _ => unreachable!(),
                })))
            })?
            .ok_or(AuraError::InvalidValue("expression terms")),
        DerivedExpressionOp::AddResidual
        | DerivedExpressionOp::SubtractResidual
        | DerivedExpressionOp::MaxPlusResidual
        | DerivedExpressionOp::MinMinusResidual
        | DerivedExpressionOp::FirstOffsetThenDelta
        | DerivedExpressionOp::PreviousSnapshotSameKeyResidual
        | DerivedExpressionOp::PreviousMutationSameKeyResidual
        | DerivedExpressionOp::PreviousOutputByKeyResidual => {
            Err(AuraError::InvalidValue("derived expression op"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(op: DerivedExpressionOp, values: &[i64], literals: &[i64]) -> Result<i64> {
        let slots = (0..values.len() as u16).collect::<Vec<_>>();
        evaluate(op, &slots, literals, |slot| Ok(values[usize::from(slot)]))
    }

    #[test]
    fn wide_intermediates_and_signed_division_preserve_integer_semantics() {
        assert_eq!(
            eval(DerivedExpressionOp::Add, &[i64::MAX, 1, -1], &[]).unwrap(),
            i64::MAX
        );
        assert_eq!(
            eval(DerivedExpressionOp::MulDiv, &[i64::MAX, 2], &[2]).unwrap(),
            i64::MAX
        );
        assert_eq!(eval(DerivedExpressionOp::Div, &[-7], &[3]).unwrap(), -2);
        assert_eq!(
            eval(DerivedExpressionOp::Min, &[i64::MAX], &[i64::MIN]).unwrap(),
            i64::MIN
        );
        assert_eq!(
            eval(DerivedExpressionOp::Max, &[i64::MIN], &[i64::MAX]).unwrap(),
            i64::MAX
        );
    }

    #[test]
    fn invalid_arithmetic_and_access_fail_closed() {
        assert!(eval(DerivedExpressionOp::Add, &[i64::MAX], &[1]).is_err());
        assert!(eval(
            DerivedExpressionOp::Mul,
            &[i64::MAX, i64::MAX, i64::MAX],
            &[]
        )
        .is_err());
        assert!(eval(DerivedExpressionOp::Div, &[1], &[0]).is_err());
        assert!(eval(DerivedExpressionOp::MulDiv, &[1], &[]).is_err());
        assert!(eval(DerivedExpressionOp::Min, &[], &[]).is_err());
        assert!(evaluate(DerivedExpressionOp::Add, &[7], &[], |_| Err(
            AuraError::InvalidValue("missing field")
        ))
        .is_err());
    }
}

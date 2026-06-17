use crate::legacy::ultra::UltraLayout;
use crate::legacy::{cold, ultra, warm, BookEvent};
use crate::Result;

pub fn cold_to_events(bytes: &[u8]) -> Result<Vec<BookEvent>> {
    cold::decode_events(bytes)
}

pub fn cold_to_warm(bytes: &[u8]) -> Result<Vec<u8>> {
    let events = cold::decode_events(bytes)?;
    warm::encode_events(&events)
}

pub fn cold_to_ultra(bytes: &[u8], layout: UltraLayout) -> Result<Vec<u8>> {
    let events = cold::decode_events(bytes)?;
    ultra::encode_events(&events, layout)
}

pub fn warm_to_ultra(bytes: &[u8], layout: UltraLayout) -> Result<Vec<u8>> {
    let events = warm::decode_events(bytes)?;
    ultra::encode_events(&events, layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{BookId, LevelChange};

    #[test]
    fn converts_cold_to_warm_and_ultra() {
        let events = vec![BookEvent::new(
            10,
            1,
            BookId::BookA,
            vec![LevelChange::new(100, 5, 0)],
            vec![LevelChange::new(101, 6, 0)],
        )];
        let cold = cold::encode_events(&events).unwrap();

        let warm_bytes = cold_to_warm(&cold).unwrap();
        let ultra_bytes = cold_to_ultra(&cold, UltraLayout::new(4).unwrap()).unwrap();

        assert_eq!(events, warm::decode_events(&warm_bytes).unwrap());
        assert_eq!(events, ultra::decode_events(&ultra_bytes).unwrap().1);
    }
}

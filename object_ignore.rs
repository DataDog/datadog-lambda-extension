use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObjectIgnore {
    Ignore,
}

impl<'de> Deserialize<'de> for ObjectIgnore {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(ObjectIgnore::Ignore)
    }
}

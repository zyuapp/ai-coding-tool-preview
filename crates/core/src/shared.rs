//! Copy-on-write values, so states that share most of their contents copy only what changed.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A value held by pointer. Cloning shares it; writing through one that is shared copies the value
/// first, so other holders keep what they saw. Equal pointers compare equal without reading.
#[derive(Default)]
pub struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    pub fn new(value: T) -> Shared<T> {
        Shared(Arc::new(value))
    }

    /// Whether both hold the same value without either having been written since.
    pub fn same(a: &Shared<T>, b: &Shared<T>) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
}

impl<T: Clone> Shared<T> {
    /// The value, copied only when someone else still holds it.
    pub fn into_inner(this: Shared<T>) -> T {
        Arc::try_unwrap(this.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

impl<T> AsRef<T> for Shared<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T> std::borrow::Borrow<T> for Shared<T> {
    fn borrow(&self) -> &T {
        &self.0
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Shared::new(value)
    }
}

impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Shared::same(self, other) || *self.0 == *other.0
    }
}

impl<T: Eq> Eq for Shared<T> {}

impl<T: PartialEq> PartialEq<T> for Shared<T> {
    fn eq(&self, other: &T) -> bool {
        *self.0 == *other
    }
}

impl<T: fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Serialize> Serialize for Shared<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Shared<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Shared::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_a_shared_value_leaves_other_holders_as_they_were() {
        let a = Shared::new(vec![1, 2]);
        let mut b = a.clone();
        assert!(Shared::same(&a, &b));
        b.push(3);
        assert_eq!(*a, vec![1, 2]);
        assert_eq!(*b, vec![1, 2, 3]);
        assert!(!Shared::same(&a, &b));
    }
}

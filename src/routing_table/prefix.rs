// Copyright 2015 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement.  This, along with the Licenses can be
// found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

use super::{XOR_NAME_BITS, XorName};
use std::cmp::{self, Ordering};
use std::fmt::{Binary, Debug, Formatter};
use std::fmt::Result as FmtResult;
use std::hash::{Hash, Hasher};
#[cfg(test)]
use std::str::FromStr;

/// A prefix with section version.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
pub struct VersionedPrefix {
    prefix: Prefix,
    version: u64,
}

impl VersionedPrefix {
    /// Returns the prefix.
    pub fn prefix(&self) -> &Prefix {
        &self.prefix
    }

    /// Returns the version number.
    pub fn version(&self) -> u64 {
        self.version
    }
}

impl Into<(Prefix, u64)> for VersionedPrefix {
    fn into(self) -> (Prefix, u64) {
        (self.prefix, self.version)
    }
}

/// A section prefix, i.e. a sequence of bits specifying the part of the network's name space
/// consisting of all names that start with this sequence.
#[derive(Clone, Copy, Default, Eq, Deserialize, Serialize)]
pub struct Prefix {
    bit_count: u16,
    name: XorName,
}

impl Prefix {
    /// Creates a new `Prefix` with the first `bit_count` bits of `name`. Insignificant bits are all
    /// set to 0. If `bit_count` exceeds the `XOR_NAME_BITS`, then it is reduced to this lower
    /// value.
    pub fn new(bit_count: usize, name: XorName) -> Prefix {
        Prefix {
            bit_count: cmp::min(bit_count, XOR_NAME_BITS) as u16,
            name: name.set_remaining(bit_count, false),
        }
    }

    /// Returns a `VersionedPrefix` with this prefix and the given version number.
    pub fn with_version(self, version: u64) -> VersionedPrefix {
        VersionedPrefix {
            prefix: self,
            version: version,
        }
    }

    /// Returns `self` with an appended bit: `0` if `bit` is `false`, and `1` if `bit` is `true`. If
    /// `self.bit_count` is already at the maximum for this type, then an unmodified copy of `self`
    /// is returned.
    pub fn pushed(mut self, bit: bool) -> Prefix {
        self.name = self.name.with_bit(self.bit_count(), bit);
        self.bit_count = cmp::min(self.bit_count + 1, XOR_NAME_BITS as u16);
        self
    }

    /// Returns a prefix copying the first `bitcount() - 1` bits from `self`,
    /// or `self` if it is already empty.
    pub fn popped(mut self) -> Prefix {
        if self.bit_count > 0 {
            self.bit_count -= 1;
            // unused bits should be zero:
            self.name = self.name.with_bit(self.bit_count(), false);
        }
        self
    }

    /// Returns the number of bits in the prefix.
    pub fn bit_count(&self) -> usize {
        self.bit_count as usize
    }

    /// Returns `true` if `self` is a prefix of `other` or vice versa.
    pub fn is_compatible(&self, other: &Prefix) -> bool {
        let i = self.name.common_prefix(&other.name);
        i >= self.bit_count() || i >= other.bit_count()
    }

    /// Returns `true` if `other` is compatible but strictly shorter than `self`.
    pub fn is_extension_of(&self, other: &Prefix) -> bool {
        let i = self.name.common_prefix(&other.name);
        i >= other.bit_count() && self.bit_count() > other.bit_count()
    }

    /// Returns `true` if the `other` prefix differs in exactly one bit from this one.
    pub fn is_neighbour(&self, other: &Prefix) -> bool {
        let i = self.name.common_prefix(&other.name);
        if i >= self.bit_count() || i >= other.bit_count() {
            false
        } else {
            let j = self.name.with_flipped_bit(i).common_prefix(&other.name);
            j >= self.bit_count() || j >= other.bit_count()
        }
    }

    /// Returns the number of common leading bits with the input name, capped with prefix length.
    pub fn common_prefix(&self, name: &XorName) -> usize {
        cmp::min(self.bit_count(), self.name.common_prefix(name))
    }

    /// Returns `true` if this is a prefix of the given `name`.
    pub fn matches(&self, name: &XorName) -> bool {
        self.name.common_prefix(name) >= self.bit_count()
    }

    /// Compares the distance of `self` and `other` to `target`. Returns `Less` if `self` is closer,
    /// `Greater` if `other` is closer, and compares the prefix directly if of equal distance
    /// (this is to make sorting deterministic).
    pub fn cmp_distance(&self, other: &Self, target: &XorName) -> Ordering {
        if self.is_compatible(other) {
            // Note that if bit_counts are equal, prefixes are also equal since
            // one is a prefix of the other (is_compatible).
            Ord::cmp(&self.bit_count, &other.bit_count)
        } else {
            Ord::cmp(
                &other.name.common_prefix(target),
                &self.name.common_prefix(target),
            )
        }
    }

    /// Returns the smallest name matching the prefix
    pub fn lower_bound(&self) -> XorName {
        self.name.set_remaining(self.bit_count(), false)
    }

    /// Returns the largest name matching the prefix
    pub fn upper_bound(&self) -> XorName {
        self.name.set_remaining(self.bit_count(), true)
    }

    /// Returns whether the namespace defined by `self` is covered by prefixes in the `prefixes`
    /// set
    pub fn is_covered_by<'a, U>(&self, prefixes: U) -> bool
    where
        XorName: 'a,
        U: IntoIterator<Item = &'a Prefix> + Clone,
    {
        let max_prefix_len = prefixes
            .clone()
            .into_iter()
            .map(|x| x.bit_count())
            .max()
            .unwrap_or(0);
        self.is_covered_by_impl(prefixes, max_prefix_len)
    }

    fn is_covered_by_impl<'a, U>(&self, prefixes: U, max_prefix_len: usize) -> bool
    where
        XorName: 'a,
        U: IntoIterator<Item = &'a Prefix> + Clone,
    {
        prefixes.clone().into_iter().any(|x| {
            x.is_compatible(self) && x.bit_count() <= self.bit_count()
        }) ||
            (self.bit_count() <= max_prefix_len &&
                 self.pushed(false).is_covered_by_impl(
                    prefixes.clone(),
                    max_prefix_len,
                ) &&
                 self.pushed(true).is_covered_by_impl(
                    prefixes,
                    max_prefix_len,
                ))
    }

    /// Returns the neighbouring prefix differing in the `i`-th bit
    /// If `i` is larger than our bit count, `self` is returned
    pub fn with_flipped_bit(&self, i: usize) -> Prefix {
        if i >= self.bit_count() {
            *self
        } else {
            Prefix::new(self.bit_count(), self.name.with_flipped_bit(i))
        }
    }

    /// Returns the given `name` with first bits replaced by `self`
    pub fn substituted_in(&self, mut name: XorName) -> XorName {
        // TODO: is there a more efficient way of doing that?
        for i in 0..self.bit_count() {
            name = name.with_bit(i, self.name.bit(i));
        }
        name
    }

    /// Returns the same prefix, with the last bit flipped, or unchanged, if empty.
    pub fn sibling(&self) -> Prefix {
        if self.bit_count > 0 {
            self.with_flipped_bit((self.bit_count - 1) as usize)
        } else {
            *self
        }
    }
}

impl PartialEq<Prefix> for Prefix {
    fn eq(&self, other: &Self) -> bool {
        self.is_compatible(other) && self.bit_count == other.bit_count
    }
}

impl PartialOrd<Prefix> for Prefix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Prefix {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            Ordering::Equal
        } else if self.is_compatible(other) {
            self.bit_count().cmp(&other.bit_count())
        } else {
            self.name.cmp(&other.name)
        }
    }
}

impl Hash for Prefix {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for i in 0..self.bit_count() {
            self.name.bit(i).hash(state);
        }
    }
}

impl Binary for Prefix {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        let mut binary = self.name.binary();
        binary.truncate(self.bit_count());
        write!(formatter, "{}", binary)
    }
}

impl Debug for Prefix {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        write!(formatter, "Prefix({:b})", self)
    }
}

#[cfg(test)]
impl FromStr for Prefix {
    type Err = String;
    fn from_str(bits: &str) -> Result<Prefix, String> {
        let mut first_byte = 0u8;
        for (i, bit) in bits.chars().enumerate() {
            if bit == '1' {
                first_byte |= 1 << (7 - i);
            } else if bit != '0' {
                return Err(format!(
                    "'{}' not allowed - the string must represent a binary number.",
                    bit
                ));
            }
        }
        let mut name = XorName::default();
        name.0[0] = first_byte;
        Ok(Prefix::new(bits.len(), name))
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use XOR_NAME_LEN;

    #[test]
    fn prefix() {
        assert_eq!(
            unwrap!(Prefix::from_str("101")).pushed(true),
            unwrap!(Prefix::from_str("1011"))
        );
        assert_eq!(
            unwrap!(Prefix::from_str("101")).pushed(false),
            unwrap!(Prefix::from_str("1010"))
        );
        assert_eq!(
            unwrap!(Prefix::from_str("1011")).popped(),
            unwrap!(Prefix::from_str("101"))
        );
        assert!(unwrap!(Prefix::from_str("101")).is_compatible(&unwrap!(
            Prefix::from_str("1010")
        )));
        assert!(unwrap!(Prefix::from_str("1010")).is_compatible(&unwrap!(
            Prefix::from_str("101")
        )));
        assert!(!unwrap!(Prefix::from_str("1010")).is_compatible(&unwrap!(
            Prefix::from_str("1011")
        )));
        assert!(unwrap!(Prefix::from_str("101")).is_neighbour(&unwrap!(
            Prefix::from_str("1111")
        )));
        assert!(!unwrap!(Prefix::from_str("1010")).is_neighbour(&unwrap!(
            Prefix::from_str("1111")
        )));
        assert!(unwrap!(Prefix::from_str("1010")).is_neighbour(&unwrap!(
            Prefix::from_str("10111")
        )));
        assert!(!unwrap!(Prefix::from_str("101")).is_neighbour(&unwrap!(
            Prefix::from_str("10111")
        )));
        let mut xor_name = XorName::default();
        xor_name[0] = 0b10101100;
        assert!(unwrap!(Prefix::from_str("101")).matches(&xor_name));
        assert!(!unwrap!(Prefix::from_str("1011")).matches(&xor_name));

        xor_name[0] = 0b01010000;
        assert_eq!(unwrap!(Prefix::from_str("0101")).lower_bound(), xor_name);
        xor_name.0 = [255; XOR_NAME_LEN];
        xor_name[0] = 0b01011111;
        assert_eq!(unwrap!(Prefix::from_str("0101")).upper_bound(), xor_name);

        // Check we handle passing an excessive `bit_count` to `new()`.
        assert_eq!(
            Prefix::new(XOR_NAME_BITS, XorName::default()).bit_count(),
            XOR_NAME_BITS
        );
        assert_eq!(
            Prefix::new(XOR_NAME_BITS + 1, XorName::default()).bit_count(),
            XOR_NAME_BITS
        );
    }
}

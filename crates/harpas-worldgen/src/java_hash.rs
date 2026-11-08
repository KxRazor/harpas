//! A set of block positions that iterates in the order of Java's `HashSet<BlockPos>`.
//!
//! Some features keep positions in a `HashSet` and walk it later, so the order in which they visit the
//! positions, and with it the blocks they place, follows `java.util.HashMap`: a table of buckets, indexed by the
//! low bits of `BlockPos.hashCode` with its high half folded in, that doubles once it holds three quarters of
//! its capacity. Each bucket keeps its positions in insertion order, and growing the table keeps the relative
//! order of the positions that share a new bucket.
//!
//! `HashMap` turns a bucket of nine or more entries into a red-black tree once the table has 64 buckets, which
//! orders that bucket differently; the bucket keeps insertion order here. Such a bucket needs nine positions of
//! one feature to share the low bits of their hash, which the few hundred blocks of a tree do not reach; equal
//! hash codes need positions 961 blocks apart.

/// Buckets before the first entry is added (`HashMap.DEFAULT_INITIAL_CAPACITY`).
const INITIAL_CAPACITY: usize = 16;
/// A bucket this long grows a small table instead of becoming a tree (`TREEIFY_THRESHOLD + 1`).
const TREEIFY_LENGTH: usize = 9;
/// The smallest table whose long buckets become trees (`MIN_TREEIFY_CAPACITY`).
const MIN_TREEIFY_CAPACITY: usize = 64;

/// `Vec3i.hashCode`.
fn hash_code([x, y, z]: [i32; 3]) -> i32 {
    y.wrapping_add(z.wrapping_mul(31))
        .wrapping_mul(31)
        .wrapping_add(x)
}

/// `HashMap.hash`: the hash code with its high half folded into the low half.
fn spread(position: [i32; 3]) -> u32 {
    let h = hash_code(position) as u32;
    h ^ (h >> 16)
}

/// A `HashSet<BlockPos>` with Java's iteration order.
#[derive(Debug, Clone, Default)]
pub struct BlockPosSet {
    table: Vec<Vec<[i32; 3]>>,
    len: usize,
}

impl BlockPosSet {
    /// An empty set (`new HashSet<>()`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of positions.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the set has no positions.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn bucket(&self, position: [i32; 3]) -> usize {
        spread(position) as usize & (self.table.len() - 1)
    }

    /// Whether the set holds a position.
    pub fn contains(&self, position: [i32; 3]) -> bool {
        !self.table.is_empty() && self.table[self.bucket(position)].contains(&position)
    }

    /// Adds a position; returns whether it was not in the set yet (`HashSet.add`).
    pub fn insert(&mut self, position: [i32; 3]) -> bool {
        if self.table.is_empty() {
            self.resize();
        }
        let index = self.bucket(position);
        let bucket = &mut self.table[index];
        if bucket.contains(&position) {
            return false;
        }
        bucket.push(position);
        if bucket.len() >= TREEIFY_LENGTH && self.table.len() < MIN_TREEIFY_CAPACITY {
            self.resize();
        }
        self.len += 1;
        if self.len > self.table.len() / 4 * 3 {
            self.resize();
        }
        true
    }

    /// Removes a position; returns whether it was in the set.
    pub fn remove(&mut self, position: [i32; 3]) -> bool {
        if self.table.is_empty() {
            return false;
        }
        let index = self.bucket(position);
        let bucket = &mut self.table[index];
        let Some(at) = bucket.iter().position(|&held| held == position) else {
            return false;
        };
        bucket.remove(at);
        self.len -= 1;
        true
    }

    /// Removes and returns the first position in iteration order (`iterator().next()`, then `remove()`).
    pub fn pop_first(&mut self) -> Option<[i32; 3]> {
        let bucket = self.table.iter_mut().find(|bucket| !bucket.is_empty())?;
        self.len -= 1;
        Some(bucket.remove(0))
    }

    /// The positions in iteration order.
    pub fn iter(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        self.table.iter().flatten().copied()
    }

    /// `HashMap.resize`: doubles the table, or creates it.
    fn resize(&mut self) {
        let capacity = if self.table.is_empty() {
            INITIAL_CAPACITY
        } else {
            self.table.len() * 2
        };
        let old = std::mem::replace(&mut self.table, vec![Vec::new(); capacity]);
        for position in old.into_iter().flatten() {
            let index = self.bucket(position);
            self.table[index].push(position);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_codes_match_vec3i() {
        assert_eq!(hash_code([0, 0, 0]), 0);
        assert_eq!(hash_code([1, 2, 3]), (2 + 3 * 31) * 31 + 1);
        assert_eq!(
            hash_code([-30_000_000, 319, 29_999_999]),
            319_i32
                .wrapping_add(29_999_999_i32.wrapping_mul(31))
                .wrapping_mul(31)
                .wrapping_sub(30_000_000)
        );
    }

    /// The iteration order Java 25 prints for a `HashSet` of positions with `Vec3i`'s hash code, after these
    /// additions and one removal.
    #[test]
    fn iterates_like_java() {
        let mut set = BlockPosSet::new();
        for i in 0..40 {
            assert!(set.insert([i % 7 - 3, 64 + i / 7, (i * 5) % 9 - 4]));
        }
        assert!(!set.insert([0, 65, 1]));
        assert!(set.remove([-3, 64, -4]));
        assert!(!set.contains([-3, 64, -4]));
        let expected = [
            [1, 68, 3],
            [-1, 66, 4],
            [2, 66, 1],
            [3, 66, -3],
            [0, 64, 2],
            [3, 64, -1],
            [-1, 64, -3],
            [3, 68, 4],
            [1, 66, -4],
            [2, 64, 3],
            [-2, 68, -3],
            [-3, 69, 0],
            [-2, 67, -2],
            [-1, 69, 1],
            [-3, 67, 2],
            [0, 67, -1],
            [-2, 65, 0],
            [1, 69, 2],
            [-1, 67, 3],
            [2, 67, 0],
            [-3, 65, 4],
            [0, 65, 1],
            [3, 65, -2],
            [1, 65, -3],
            [1, 67, 4],
            [2, 65, 2],
            [3, 67, -4],
            [-1, 65, -4],
            [0, 69, -3],
            [-2, 69, -4],
            [-3, 68, 1],
            [0, 68, -2],
            [-2, 66, -1],
            [-1, 68, 2],
            [2, 68, -1],
            [-3, 66, 3],
            [0, 66, 0],
            [-2, 64, 1],
            [1, 64, -2],
        ];
        assert_eq!(set.iter().collect::<Vec<_>>(), expected);
        assert_eq!(set.pop_first(), Some([1, 68, 3]));
        assert_eq!(set.len(), expected.len() - 1);
    }

    /// A ninth entry in one bucket of a 16-bucket table doubles the table, as Java 25 does.
    #[test]
    fn long_buckets_grow_small_tables() {
        let mut set = BlockPosSet::new();
        for i in 0..11 {
            set.insert([i * 16, 0, 0]);
        }
        set.insert([1, 0, 0]);
        let order: Vec<i32> = set.iter().map(|[x, _, _]| x).collect();
        assert_eq!(order, [0, 32, 64, 96, 128, 160, 1, 16, 48, 80, 112, 144]);
    }
}

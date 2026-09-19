pub use std::collections::{BTreeMap, BTreeSet, hash_map};

// We don't need or want the random state of the default std collections.
// We also don't need cryptographic hashing, and these are faster.
pub use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};


// Copyright 2017, 2021 Parity Technologies
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
#![cfg_attr(not(feature = "std"), no_std)]

//! Trie interface and implementation.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(feature = "std")]
mod rstd {
	pub use std::{
		borrow, boxed, cmp, collections::VecDeque, convert, error::Error, fmt, hash, iter, marker,
		mem, ops, rc, result, sync, vec,
	};
}

#[cfg(not(feature = "std"))]
mod rstd {
	pub use alloc::{borrow, boxed, collections::VecDeque, rc, sync, vec};
	pub use core::{cmp, convert, fmt, hash, iter, marker, mem, ops, result};
	pub trait Error {}
	impl<T> Error for T {}
}

#[cfg(feature = "std")]
use self::rstd::{fmt, Error};

use self::rstd::{boxed::Box, vec::Vec};
use hash_db::MaybeDebug;
use node::NodeOwned;

pub mod node;
pub mod proof;
pub mod recorder;
pub mod sectriedb;
pub mod sectriedbmut;
pub mod triedb;
pub mod triedbmut;

mod fatdb;
mod fatdbmut;
mod iter_build;
mod iterator;
mod lookup;
mod nibble;
mod node_codec;
mod trie_codec;

pub use self::{
	fatdb::{FatDB, FatDBIterator},
	fatdbmut::FatDBMut,
	lookup::Lookup,
	nibble::{nibble_ops, NibbleSlice, NibbleVec},
	recorder::Recorder,
	sectriedb::SecTrieDB,
	sectriedbmut::SecTrieDBMut,
	triedb::{TrieDB, TrieDBBuilder, TrieDBIterator, TrieDBKeyIterator},
	triedbmut::{ChildReference, TrieDBMut, TrieDBMutBuilder, Value},
};
pub use crate::{
	iter_build::{trie_visit, ProcessEncodedNode, TrieBuilder, TrieRoot, TrieRootUnhashed},
	iterator::TrieDBNodeIterator,
	node_codec::{NodeCodec, Partial},
	trie_codec::{decode_compact, decode_compact_from_iter, encode_compact},
};
pub use hash_db::{HashDB, HashDBRef, Hasher};

#[cfg(feature = "std")]
pub use crate::iter_build::TrieRootPrint;

/// Database value
pub type DBValue = Vec<u8>;

/// Trie Errors.
///
/// These borrow the data within them to avoid excessive copying on every
/// trie operation.
#[derive(PartialEq, Eq, Clone, Debug)]
pub enum TrieError<T, E> {
	/// Attempted to create a trie with a state root not in the DB.
	InvalidStateRoot(T),
	/// Trie item not found in the database,
	IncompleteDatabase(T),
	/// A value was found in the trie with a nibble key that was not byte-aligned.
	/// The first parameter is the byte-aligned part of the prefix and the second parameter is the
	/// remaining nibble.
	ValueAtIncompleteKey(Vec<u8>, u8),
	/// Corrupt Trie item.
	DecoderError(T, E),
	/// Hash is not value.
	InvalidHash(T, Vec<u8>),
}

#[cfg(feature = "std")]
impl<T, E> fmt::Display for TrieError<T, E>
where
	T: MaybeDebug,
	E: MaybeDebug,
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			TrieError::InvalidStateRoot(ref root) => write!(f, "Invalid state root: {:?}", root),
			TrieError::IncompleteDatabase(ref missing) =>
				write!(f, "Database missing expected key: {:?}", missing),
			TrieError::ValueAtIncompleteKey(ref bytes, ref extra) =>
				write!(f, "Value found in trie at incomplete key {:?} + {:?}", bytes, extra),
			TrieError::DecoderError(ref hash, ref decoder_err) => {
				write!(f, "Decoding failed for hash {:?}; err: {:?}", hash, decoder_err)
			},
			TrieError::InvalidHash(ref hash, ref data) => write!(
				f,
				"Encoded node {:?} contains invalid hash reference with length: {}",
				hash,
				data.len()
			),
		}
	}
}

#[cfg(feature = "std")]
impl<T, E> Error for TrieError<T, E>
where
	T: fmt::Debug,
	E: Error,
{
}

/// Trie result type.
/// Boxed to avoid copying around extra space for the `Hasher`s `Out` on successful queries.
pub type Result<T, H, E> = crate::rstd::result::Result<T, Box<TrieError<H, E>>>;

/// Trie-Item type used for iterators over trie data.
pub type TrieItem<U, E> = Result<(Vec<u8>, DBValue), U, E>;

/// Trie-Item type used for iterators over trie key only.
pub type TrieKeyItem<U, E> = Result<Vec<u8>, U, E>;

/// Description of what kind of query will be made to the trie.
///
/// This is implemented for any &mut recorder (where the query will return
/// a DBValue), any function taking raw bytes (where no recording will be made),
/// or any tuple of (&mut Recorder, FnOnce(&[u8]))
pub trait Query<H: Hasher> {
	/// Output item.
	type Item;

	/// Decode a byte-slice into the desired item.
	fn decode(self, data: &[u8]) -> Self::Item;
}

/// Used to report the trie access to the [`TrieRecorder`].
///
/// As the trie can use a [`TrieCache`], there are multiple kinds of accesses.
/// If a cache is used, [`Self::Key`] and [`Self::NodeOwned`] are possible
/// values. Otherwise only [`Self::EncodedNode`] is a possible value.
#[cfg_attr(feature = "std", derive(Debug))]
pub enum TrieAccess<'a, H> {
	/// The given `key` was accessed and the cache answered the request with the given `value`.
	///
	/// This is no real "trie access", but it needs to be recorded. The
	/// recorder needs to ensure that it fetches the trie nodes to access
	/// the data under the given `key` before it returns all accessed
	/// nodes to the user.
	Key { key: &'a [u8], value: Option<rstd::borrow::Cow<'a, [u8]>> },
	/// The given [`NodeOwned`] was accessed using its `hash`.
	NodeOwned { hash: H, node_owned: &'a NodeOwned<H> },
	/// The given `encoded_node` was accessed using its `hash`.
	EncodedNode { hash: H, encoded_node: rstd::borrow::Cow<'a, [u8]> },
	/// The given `value` was accessed using its `hash`.
	///
	/// The given `full_key` is the key to access this value in the trie.
	Value { hash: H, value: rstd::borrow::Cow<'a, [u8]>, full_key: &'a [u8] },
}

/// A trie recorder that can be used to record all kind of trie accesses.
pub trait TrieRecorder<H> {
	/// Record the given [`TrieAccess`].
	fn record<'a>(&mut self, access: TrieAccess<'a, H>);
}

impl<T: TrieRecorder<H> + ?Sized, H> TrieRecorder<H> for Option<&mut T> {
	fn record<'a>(&mut self, access: TrieAccess<'a, H>) {
		if let Some(ref mut recorder) = self {
			recorder.record(access);
		}
	}
}

impl<F, T, H: Hasher> Query<H> for F
where
	F: for<'a> FnOnce(&'a [u8]) -> T,
{
	type Item = T;
	fn decode(self, value: &[u8]) -> T {
		(self)(value)
	}
}

/// A key-value datastore implemented as a database-backed modified Merkle tree.
pub trait Trie<L: TrieLayout> {
	/// Return the root of the trie.
	fn root(&self) -> &TrieHash<L>;

	/// Is the trie empty?
	fn is_empty(&self) -> bool {
		*self.root() == L::Codec::hashed_null_node()
	}

	/// Does the trie contain a given key?
	fn contains(&self, key: &[u8]) -> Result<bool, TrieHash<L>, CError<L>> {
		self.get(key).map(|x| x.is_some())
	}

	/// What is the value of the given key in this trie?
	fn get<'a, 'key>(&'a self, key: &'key [u8]) -> Result<Option<DBValue>, TrieHash<L>, CError<L>>
	where
		'a: 'key,
	{
		self.get_with(key, |v: &[u8]| v.to_vec())
	}

	/// Search for the key with the given query parameter. See the docs of the `Query`
	/// trait for more details.
	fn get_with<Q: Query<L::Hash>>(
		&self,
		key: &[u8],
		query: Q,
	) -> Result<Option<Q::Item>, TrieHash<L>, CError<L>>;

	/// Returns a depth-first iterator over the elements of trie.
	fn iter<'a>(
		&'a self,
	) -> Result<
		Box<dyn TrieIterator<L, Item = TrieItem<TrieHash<L>, CError<L>>> + 'a>,
		TrieHash<L>,
		CError<L>,
	>;

	/// Returns a depth-first iterator over the keys of elemets of trie.
	fn key_iter<'a>(
		&'a self,
	) -> Result<
		Box<dyn TrieIterator<L, Item = TrieKeyItem<TrieHash<L>, CError<L>>> + 'a>,
		TrieHash<L>,
		CError<L>,
	>;
}

/// A key-value datastore implemented as a database-backed modified Merkle tree.
pub trait TrieMut<L: TrieLayout> {
	/// Return the root of the trie.
	fn root(&mut self) -> &TrieHash<L>;

	/// Is the trie empty?
	fn is_empty(&self) -> bool;

	/// Does the trie contain a given key?
	fn contains(&self, key: &[u8]) -> Result<bool, TrieHash<L>, CError<L>> {
		self.get(key).map(|x| x.is_some())
	}

	/// What is the value of the given key in this trie?
	fn get<'a, 'key>(&'a self, key: &'key [u8]) -> Result<Option<DBValue>, TrieHash<L>, CError<L>>
	where
		'a: 'key;

	/// Insert a `key`/`value` pair into the trie. An empty value is equivalent to removing
	/// `key` from the trie. Returns the old value associated with this key, if it existed.
	fn insert(
		&mut self,
		key: &[u8],
		value: &[u8],
	) -> Result<Option<Value<L>>, TrieHash<L>, CError<L>>;

	/// Remove a `key` from the trie. Equivalent to making it equal to the empty
	/// value. Returns the old value associated with this key, if it existed.
	fn remove(&mut self, key: &[u8]) -> Result<Option<Value<L>>, TrieHash<L>, CError<L>>;
}

/// A trie iterator that also supports random access (`seek()`).
pub trait TrieIterator<L: TrieLayout>: Iterator {
	/// Position the iterator on the first element with key >= `key`
	fn seek(&mut self, key: &[u8]) -> Result<(), TrieHash<L>, CError<L>>;
}

/// Trie types
#[derive(PartialEq, Clone)]
#[cfg_attr(feature = "std", derive(Debug))]
pub enum TrieSpec {
	/// Generic trie.
	Generic,
	/// Secure trie.
	Secure,
	///	Secure trie with fat database.
	Fat,
}

impl Default for TrieSpec {
	fn default() -> TrieSpec {
		TrieSpec::Secure
	}
}

/// Trie factory.
#[derive(Default, Clone)]
pub struct TrieFactory<L: TrieLayout> {
	spec: TrieSpec,
	_phantom: rstd::marker::PhantomData<L>,
}

/// All different kinds of tries.
/// This is used to prevent a heap allocation for every created trie.
pub enum TrieKinds<'db, 'cache, L: TrieLayout> {
	/// A generic trie db.
	Generic(TrieDB<'db, 'cache, L>),
	/// A secure trie db.
	Secure(SecTrieDB<'db, 'cache, L>),
	/// A fat trie db.
	Fat(FatDB<'db, 'cache, L>),
}

// wrapper macro for making the match easier to deal with.
macro_rules! wrapper {
	($me: ident, $f_name: ident, $($param: ident),*) => {
		match *$me {
			TrieKinds::Generic(ref t) => t.$f_name($($param),*),
			TrieKinds::Secure(ref t) => t.$f_name($($param),*),
			TrieKinds::Fat(ref t) => t.$f_name($($param),*),
		}
	}
}

impl<'db, 'cache, L: TrieLayout> Trie<L> for TrieKinds<'db, 'cache, L> {
	fn root(&self) -> &TrieHash<L> {
		wrapper!(self, root,)
	}

	fn is_empty(&self) -> bool {
		wrapper!(self, is_empty,)
	}

	fn contains(&self, key: &[u8]) -> Result<bool, TrieHash<L>, CError<L>> {
		wrapper!(self, contains, key)
	}

	fn get_with<Q: Query<L::Hash>>(
		&self,
		key: &[u8],
		query: Q,
	) -> Result<Option<Q::Item>, TrieHash<L>, CError<L>> {
		wrapper!(self, get_with, key, query)
	}

	fn iter<'a>(
		&'a self,
	) -> Result<
		Box<dyn TrieIterator<L, Item = TrieItem<TrieHash<L>, CError<L>>> + 'a>,
		TrieHash<L>,
		CError<L>,
	> {
		wrapper!(self, iter,)
	}

	fn key_iter<'a>(
		&'a self,
	) -> Result<
		Box<dyn TrieIterator<L, Item = TrieKeyItem<TrieHash<L>, CError<L>>> + 'a>,
		TrieHash<L>,
		CError<L>,
	> {
		wrapper!(self, key_iter,)
	}
}

impl<'db, L> TrieFactory<L>
where
	L: TrieLayout + 'db,
{
	/// Creates new factory.
	pub fn new(spec: TrieSpec) -> Self {
		TrieFactory { spec, _phantom: Default::default() }
	}

	/// Create new immutable instance of Trie.
	pub fn readonly<'cache>(
		&self,
		db: &'db dyn HashDBRef<L::Hash, DBValue>,
		root: &'db TrieHash<L>,
	) -> Result<TrieKinds<'db, 'cache, L>, TrieHash<L>, CError<L>> {
		match self.spec {
			TrieSpec::Generic => Ok(TrieKinds::Generic(TrieDBBuilder::new(db, root)?.build())),
			TrieSpec::Secure => Ok(TrieKinds::Secure(SecTrieDB::new(db, root)?)),
			TrieSpec::Fat => Ok(TrieKinds::Fat(FatDB::new(db, root)?)),
		}
	}

	/// Create new mutable instance of Trie.
	pub fn create(
		&self,
		db: &'db mut dyn HashDB<L::Hash, DBValue>,
		root: &'db mut TrieHash<L>,
	) -> Box<dyn TrieMut<L> + 'db> {
		match self.spec {
			TrieSpec::Generic => Box::new(TrieDBMutBuilder::<L>::new(db, root).build()),
			TrieSpec::Secure => Box::new(SecTrieDBMut::<L>::new(db, root)),
			TrieSpec::Fat => Box::new(FatDBMut::<L>::new(db, root)),
		}
	}

	/// Create new mutable instance of trie and check for errors.
	pub fn from_existing(
		&self,
		db: &'db mut dyn HashDB<L::Hash, DBValue>,
		root: &'db mut TrieHash<L>,
	) -> Result<Box<dyn TrieMut<L> + 'db>, TrieHash<L>, CError<L>> {
		match self.spec {
			TrieSpec::Generic =>
				Ok(Box::new(TrieDBMutBuilder::<L>::from_existing(db, root)?.build())),
			TrieSpec::Secure => Ok(Box::new(SecTrieDBMut::<L>::from_existing(db, root)?)),
			TrieSpec::Fat => Ok(Box::new(FatDBMut::<L>::from_existing(db, root)?)),
		}
	}

	/// Returns true iff the trie DB is a fat DB (allows enumeration of keys).
	pub fn is_fat(&self) -> bool {
		self.spec == TrieSpec::Fat
	}
}

/// Trait with definition of trie layout.
/// Contains all associated trait needed for
/// a trie definition or implementation.
pub trait TrieLayout {
	/// If true, the trie will use extension nodes and
	/// no partial in branch, if false the trie will only
	/// use branch and node with partials in both.
	const USE_EXTENSION: bool;
	/// If true, the trie will allow empty values into `TrieDBMut`
	const ALLOW_EMPTY: bool = false;
	/// Threshold above which an external node should be
	/// use to store a node value.
	const MAX_INLINE_VALUE: Option<u32>;

	/// Hasher to use for this trie.
	type Hash: Hasher;
	/// Codec to use (needs to match hasher and nibble ops).
	type Codec: NodeCodec<HashOut = <Self::Hash as Hasher>::Out>;
}

/// This trait associates a trie definition with preferred methods.
/// It also contains own default implementations and can be
/// used to allow switching implementation.
pub trait TrieConfiguration: Sized + TrieLayout {
	/// Operation to build a trie db from its ordered iterator over its key/values.
	fn trie_build<DB, I, A, B>(db: &mut DB, input: I) -> <Self::Hash as Hasher>::Out
	where
		DB: HashDB<Self::Hash, DBValue>,
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
	{
		let mut cb = TrieBuilder::<Self, DB>::new(db);
		trie_visit::<Self, _, _, _, _>(input.into_iter(), &mut cb);
		cb.root.unwrap_or_default()
	}
	/// Determines a trie root given its ordered contents, closed form.
	fn trie_root<I, A, B>(input: I) -> <Self::Hash as Hasher>::Out
	where
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
	{
		let mut cb = TrieRoot::<Self>::default();
		trie_visit::<Self, _, _, _, _>(input.into_iter(), &mut cb);
		cb.root.unwrap_or_default()
	}
	/// Determines a trie root node's data given its ordered contents, closed form.
	fn trie_root_unhashed<I, A, B>(input: I) -> Vec<u8>
	where
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
	{
		let mut cb = TrieRootUnhashed::<Self>::default();
		trie_visit::<Self, _, _, _, _>(input.into_iter(), &mut cb);
		cb.root.unwrap_or_default()
	}
	/// Encoding of index as a key (when reusing general trie for
	/// indexed trie).
	fn encode_index(input: u32) -> Vec<u8> {
		// be for byte ordering
		input.to_be_bytes().to_vec()
	}
	/// A trie root formed from the items, with keys attached according to their
	/// compact-encoded index (using `parity-codec` crate).
	fn ordered_trie_root<I, A>(input: I) -> <Self::Hash as Hasher>::Out
	where
		I: IntoIterator<Item = A>,
		A: AsRef<[u8]>,
	{
		Self::trie_root(
			input.into_iter().enumerate().map(|(i, v)| (Self::encode_index(i as u32), v)),
		)
	}
}

/// Alias accessor to hasher hash output type from a `TrieLayout`.
pub type TrieHash<L> = <<L as TrieLayout>::Hash as Hasher>::Out;
/// Alias accessor to `NodeCodec` associated `Error` type from a `TrieLayout`.
pub type CError<L> = <<L as TrieLayout>::Codec as NodeCodec>::Error;

/// A value as cached by the [`TrieCache`].
#[derive(Clone)]
pub struct CachedValue<H> {
	/// The hash of the value.
	pub hash: H,
	/// The actual data of the value stored as [`BytesWeak`].
	///
	/// The original data [`Bytes`] is stored in the trie node
	/// that is also cached by the [`TrieCache`]. If this node is dropped,
	/// this data will also not be "upgradeable" anymore.
	pub data: BytesWeak,
}

impl<H: Copy> CachedValue<H> {
	/// Upgrade this cached value to the actual data and hash.
	///
	/// As `data` is stored as [`BytesWeak`] we first need to upgrade
	/// it to the actual [`Bytes`] and as this can fails this function
	/// returns an [`Option`].
	pub fn upgrade(&self) -> Option<(Bytes, H)> {
		self.data.upgrade().map(|b| (b, self.hash))
	}
}

impl<H> From<(Bytes, H)> for CachedValue<H> {
	fn from(value: (Bytes, H)) -> Self {
		Self {
			hash: value.1,
			data: value.0.into(),
		}
	}
}

/// A cache that can be used to speed-up certain operations when accessing the trie.
pub trait TrieCache<NC: NodeCodec> {
	/// Lookup value for the given `key`.
	///
	/// Returns the `None` if the `key` is unknown or otherwise `Some(_)` with the associated
	/// value.
	///
	/// [`Self::cache_data_for_key`] is used to make the cache aware of data that is associated
	/// to a `key`.
	///
	/// # Attention
	///
	/// The cache can be used for different tries, aka with different roots. This means
	/// that the cache implementation needs to take care of always returning the correct value
	/// for the current trie root.
	fn lookup_value_for_key(&self, key: &[u8]) -> Option<&Option<CachedValue<NC::HashOut>>>;

	/// Cache the given `value` for the given `key`.
	///
	/// If the given `key` could not be found in the trie, `None` will be passed for `value`.
	///
	/// # Attention
	///
	/// The cache can be used for different tries, aka with different roots. This means
	/// that the cache implementation needs to take care of caching `value` for the current
	/// trie root.
	fn cache_value_for_key(&mut self, key: &[u8], value: Option<CachedValue<NC::HashOut>>);

	/// Get or insert a [`NodeOwned`].
	///
	/// The cache implementation should look up based on the given `hash` if the node is already
	/// known. If the node is not yet known, the given `fetch_node` function can be used to fetch
	/// the particular node.
	///
	/// Returns the [`NodeOwned`] or an error that happened on fetching the node.
	fn get_or_insert_node(
		&mut self,
		hash: NC::HashOut,
		fetch_node: &mut dyn FnMut() -> Result<NodeOwned<NC::HashOut>, NC::HashOut, NC::Error>,
	) -> Result<&NodeOwned<NC::HashOut>, NC::HashOut, NC::Error>;

	/// Insert the given [`OwnedNode`] under the given `hash`.
	fn insert_node(&mut self, hash: NC::HashOut, node: NodeOwned<NC::HashOut>);

	/// Get the [`OwnedNode`] that corresponds to the given `hash`.
	fn get_node(&mut self, hash: &NC::HashOut) -> Option<&NodeOwned<NC::HashOut>>;
}

/// A container for storing bytes.
///
/// This is internally uses a reference counted pointer, so it is cheap to clone this object.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(rstd::sync::Arc<[u8]>);

impl rstd::ops::Deref for Bytes {
	type Target = [u8];

	fn deref(&self) -> &Self::Target {
		self.0.deref()
	}
}

impl From<Vec<u8>> for Bytes {
	fn from(bytes: Vec<u8>) -> Self {
		Self(bytes.into())
	}
}

impl From<&[u8]> for Bytes {
	fn from(bytes: &[u8]) -> Self {
		Self(bytes.into())
	}
}

/// A weak reference of [`Bytes`].
///
/// A weak reference means that it doesn't prevent [`Bytes`] of being dropped because
/// it holds a non-owning reference to the associated [`Bytes`] object. With [`Self::upgrade`] it
/// is possible to upgrade it again to [`Bytes`] if the reference is still valid.
#[derive(Clone, Debug)]
pub struct BytesWeak(rstd::sync::Weak<[u8]>);

impl BytesWeak {
	/// Upgrade to [`Bytes`].
	///
	/// Returns `None` when the inner value was already dropped.
	pub fn upgrade(&self) -> Option<Bytes> {
		self.0.upgrade().map(Bytes)
	}
}

impl From<Bytes> for BytesWeak {
	fn from(bytes: Bytes) -> Self {
		Self(rstd::sync::Arc::downgrade(&bytes.0))
	}
}

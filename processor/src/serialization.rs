use std::cmp::max;
use std::collections::HashMap;

use bitcoin::BlockHash;
use lightning::ln::msgs::{OptionalField, UnsignedChannelAnnouncement, UnsignedChannelUpdate};
use lightning::util::ser::{BigSize, Writeable};

use crate::lookup::{DeltaSet, DirectedUpdateDelta};

pub(super) struct SerializationSet {
	pub(super) announcements: Vec<UnsignedChannelAnnouncement>,
	pub(super) updates: Vec<UpdateSerialization>,
	pub(super) full_update_defaults: DefaultUpdateValues,
	pub(super) latest_seen: u32,
	pub(super) chain_hash: BlockHash,
}

pub(super) struct DefaultUpdateValues {
	pub(super) cltv_expiry_delta: u16,
	pub(super) htlc_minimum_msat: u64,
	pub(super) fee_base_msat: u32,
	pub(super) fee_proportional_millionths: u32,
	pub(super) htlc_maximum_msat: u64,
}

impl Default for DefaultUpdateValues {
	fn default() -> Self {
		Self {
			cltv_expiry_delta: 0,
			htlc_minimum_msat: 0,
			fee_base_msat: 0,
			fee_proportional_millionths: 0,
			htlc_maximum_msat: 0,
		}
	}
}

pub(super) struct UpdateChangeSet {
	pub(super) affected_field_count: u8,
	pub(super) affected_fields: Vec<String>,
	pub(super) serialization: Vec<u8>,
}

pub(super) struct MutatedProperties {
	flags: bool,
	cltv_expiry_delta: bool,
	htlc_minimum_msat: bool,
	fee_base_msat: bool,
	fee_proportional_millionths: bool,
	htlc_maximum_msat: bool,
}

impl Default for MutatedProperties {
	fn default() -> Self {
		Self {
			flags: false,
			cltv_expiry_delta: false,
			htlc_minimum_msat: false,
			fee_base_msat: false,
			fee_proportional_millionths: false,
			htlc_maximum_msat: false,
		}
	}
}

pub(super) struct UpdateSerialization {
	pub(super) update: UnsignedChannelUpdate,
	pub(super) mechanism: UpdateSerializationMechanism,
}

impl MutatedProperties {
	/// Does not include flags because the flag byte is always sent in full
	fn len(&self) -> u8 {
		let mut mutations = 0;
		if self.cltv_expiry_delta { mutations += 1; };
		if self.htlc_minimum_msat { mutations += 1; };
		if self.fee_base_msat { mutations += 1; };
		if self.fee_proportional_millionths { mutations += 1; };
		if self.htlc_maximum_msat { mutations += 1; };
		mutations
	}
}

pub(super) enum UpdateSerializationMechanism {
	Full,
	Incremental(MutatedProperties),
}

struct FullUpdateValueHistograms {
	cltv_expiry_delta: HashMap<u16, usize>,
	htlc_minimum_msat: HashMap<u64, usize>,
	fee_base_msat: HashMap<u32, usize>,
	fee_proportional_millionths: HashMap<u32, usize>,
	htlc_maximum_msat: HashMap<u64, usize>,
}

pub(super) fn serialize_delta_set(delta_set: DeltaSet, last_sync_timestamp: u32, consider_intermediate_updates: bool) -> SerializationSet {
	let mut serialization_set = SerializationSet {
		announcements: vec![],
		updates: vec![],
		full_update_defaults: Default::default(),
		chain_hash: Default::default(),
		latest_seen: 0,
	};

	let mut chain_hash_set = false;

	let mut full_update_histograms = FullUpdateValueHistograms {
		cltv_expiry_delta: Default::default(),
		htlc_minimum_msat: Default::default(),
		fee_base_msat: Default::default(),
		fee_proportional_millionths: Default::default(),
		htlc_maximum_msat: Default::default(),
	};

	let mut record_full_update_in_histograms = |full_update: &UnsignedChannelUpdate| {
		*full_update_histograms.cltv_expiry_delta.entry(full_update.cltv_expiry_delta).or_insert(0) += 1;
		*full_update_histograms.htlc_minimum_msat.entry(full_update.htlc_minimum_msat).or_insert(0) += 1;
		*full_update_histograms.fee_base_msat.entry(full_update.fee_base_msat).or_insert(0) += 1;
		*full_update_histograms.fee_proportional_millionths.entry(full_update.fee_proportional_millionths).or_insert(0) += 1;
		let htlc_maximum_msat_key = optional_htlc_maximum_to_u64(&full_update.htlc_maximum_msat);
		*full_update_histograms.htlc_maximum_msat.entry(htlc_maximum_msat_key).or_insert(0) += 1;
	};

	// delta_set.into_iter().is_sorted_by_key()
	for (_scid, channel_delta) in delta_set.into_iter() {

		// any announcement chain hash is gonna be the same value. Just set it from the first one.
		let channel_announcement_delta = channel_delta.announcement.as_ref().unwrap();
		if !chain_hash_set {
			chain_hash_set = true;
			serialization_set.chain_hash = channel_announcement_delta.announcement.chain_hash.clone();
		}

		let current_announcement_seen = channel_announcement_delta.seen;
		let is_new_announcement = current_announcement_seen >= last_sync_timestamp;
		let is_newly_updated_announcement = if let Some(first_update_seen) = channel_delta.first_update_seen {
			first_update_seen >= last_sync_timestamp
		} else {
			false
		};
		let send_announcement = is_new_announcement || is_newly_updated_announcement;
		if send_announcement {
			serialization_set.latest_seen = max(serialization_set.latest_seen, current_announcement_seen);
			serialization_set.announcements.push(channel_delta.announcement.unwrap().announcement);
		}

		let direction_a_updates = channel_delta.updates.0;
		let direction_b_updates = channel_delta.updates.1;

		let mut categorize_directed_update_serialization = |directed_updates: Option<DirectedUpdateDelta>| {
			if let Some(updates) = directed_updates {
				if let Some(latest_update_delta) = updates.latest_update_after_seen {
					let latest_update = latest_update_delta.update;

					// the returned seen timestamp should be the latest of all the returned
					// announcements and latest updates
					serialization_set.latest_seen = max(serialization_set.latest_seen, latest_update_delta.seen);

					if let Some(last_seen_update) = updates.last_update_before_seen {

						// we typically compare only the latest update with the last seen
						let mut compared_updates = vec![last_seen_update];
						if consider_intermediate_updates && !updates.intermediate_updates.is_empty() {
							// however, if intermediate updates are to be considered,
							// they are all included
							compared_updates.append(&mut updates.intermediate_updates.clone());
						}
						compared_updates.push(latest_update.clone());

						let mut mutated_properties = MutatedProperties::default();
						for i in 1..compared_updates.len() {
							let previous_update = &compared_updates[i - 1];
							let current_update = &compared_updates[i];
							if current_update.flags != previous_update.flags {
								mutated_properties.flags = true;
							}
							if current_update.cltv_expiry_delta != previous_update.cltv_expiry_delta {
								mutated_properties.cltv_expiry_delta = true;
							}
							if current_update.htlc_minimum_msat != previous_update.htlc_minimum_msat {
								mutated_properties.htlc_minimum_msat = true;
							}
							if current_update.fee_base_msat != previous_update.fee_base_msat {
								mutated_properties.fee_base_msat = true;
							}
							if current_update.fee_proportional_millionths != previous_update.fee_proportional_millionths {
								mutated_properties.fee_proportional_millionths = true;
							}
							if current_update.htlc_maximum_msat != previous_update.htlc_maximum_msat {
								mutated_properties.htlc_maximum_msat = true;
							}
						};
						if mutated_properties.len() == 5 {
							// all five values have changed, it makes more sense to just
							// serialize the update as a full update instead of as a change
							// this way, the default values can be computed more efficiently
							record_full_update_in_histograms(&latest_update);
							serialization_set.updates.push(UpdateSerialization {
								update: latest_update,
								mechanism: UpdateSerializationMechanism::Full,
							});
						} else if mutated_properties.len() > 0 || mutated_properties.flags {
							// we don't count flags as mutated properties
							serialization_set.updates.push(UpdateSerialization {
								update: latest_update,
								mechanism: UpdateSerializationMechanism::Incremental(mutated_properties),
							});
						}
					} else {
						// serialize the full update
						record_full_update_in_histograms(&latest_update);
						serialization_set.updates.push(UpdateSerialization {
							update: latest_update,
							mechanism: UpdateSerializationMechanism::Full,
						});
					}
				}
			};
		};

		categorize_directed_update_serialization(direction_a_updates);
		categorize_directed_update_serialization(direction_b_updates);
	}

	let default_update_values = DefaultUpdateValues {
		cltv_expiry_delta: find_most_common_histogram_entry_with_default(full_update_histograms.cltv_expiry_delta, 0),
		htlc_minimum_msat: find_most_common_histogram_entry_with_default(full_update_histograms.htlc_minimum_msat, 0),
		fee_base_msat: find_most_common_histogram_entry_with_default(full_update_histograms.fee_base_msat, 0),
		fee_proportional_millionths: find_most_common_histogram_entry_with_default(full_update_histograms.fee_proportional_millionths, 0),
		htlc_maximum_msat: find_most_common_histogram_entry_with_default(full_update_histograms.htlc_maximum_msat, 0),
	};

	serialization_set.full_update_defaults = default_update_values;
	serialization_set
}

pub fn serialize_stripped_channel_announcement(announcement: &UnsignedChannelAnnouncement, node_id_a_index: usize, node_id_b_index: usize, previous_scid: u64) -> Vec<u8> {
	let mut stripped_announcement = vec![];
	announcement.features.write(&mut stripped_announcement);

	if previous_scid > announcement.short_channel_id {
		panic!("unsorted scids!");
	}
	let scid_delta = BigSize(announcement.short_channel_id - previous_scid);
	scid_delta.write(&mut stripped_announcement);

	// write indices of node ids rather than the node IDs themselves
	BigSize(node_id_a_index as u64).write(&mut stripped_announcement);
	BigSize(node_id_b_index as u64).write(&mut stripped_announcement);

	// println!("serialized CA: {}, \n{:?}\n{:?}\n", announcement.short_channel_id, announcement.node_id_1, announcement.node_id_2);
	stripped_announcement
}

pub(super) fn serialize_stripped_channel_update(update: &UpdateSerialization, default_values: &DefaultUpdateValues, previous_scid: u64) -> Vec<u8> {
	let latest_update = &update.update;
	let mut serialized_flags = latest_update.flags;

	if previous_scid > latest_update.short_channel_id {
		panic!("unsorted scids!");
	}

	let mut delta_serialization = Vec::new();
	let mut prefixed_serialization = Vec::new();
	match &update.mechanism {
		UpdateSerializationMechanism::Full => {
			if latest_update.cltv_expiry_delta != default_values.cltv_expiry_delta {
				serialized_flags |= 0b_0100_0000;
				latest_update.cltv_expiry_delta.write(&mut delta_serialization).unwrap();
			}

			if latest_update.htlc_minimum_msat != default_values.htlc_minimum_msat {
				serialized_flags |= 0b_0010_0000;
				latest_update.htlc_minimum_msat.write(&mut delta_serialization).unwrap();
			}

			if latest_update.fee_base_msat != default_values.fee_base_msat {
				serialized_flags |= 0b_0001_0000;
				latest_update.fee_base_msat.write(&mut delta_serialization).unwrap();
			}

			if latest_update.fee_proportional_millionths != default_values.fee_proportional_millionths {
				serialized_flags |= 0b_0000_1000;
				latest_update.fee_proportional_millionths.write(&mut delta_serialization).unwrap();
			}

			let latest_update_htlc_maximum = optional_htlc_maximum_to_u64(&latest_update.htlc_maximum_msat);
			if latest_update_htlc_maximum != default_values.htlc_maximum_msat {
				serialized_flags |= 0b_0000_0100;
				latest_update_htlc_maximum.write(&mut delta_serialization).unwrap();
			}
		}

		UpdateSerializationMechanism::Incremental(mutated_properties) => {
			// indicate that this update is incremental
			serialized_flags |= 0b_1000_0000;

			if mutated_properties.cltv_expiry_delta {
				serialized_flags |= 0b_0100_0000;
				latest_update.cltv_expiry_delta.write(&mut delta_serialization).unwrap();
			}

			if mutated_properties.htlc_minimum_msat {
				serialized_flags |= 0b_0010_0000;
				latest_update.htlc_minimum_msat.write(&mut delta_serialization).unwrap();
			}

			if mutated_properties.fee_base_msat {
				serialized_flags |= 0b_0001_0000;
				latest_update.fee_base_msat.write(&mut delta_serialization).unwrap();
			}

			if mutated_properties.fee_proportional_millionths {
				serialized_flags |= 0b_0000_1000;
				latest_update.fee_proportional_millionths.write(&mut delta_serialization).unwrap();
			}

			if mutated_properties.htlc_maximum_msat {
				serialized_flags |= 0b_0000_0100;

				let new_htlc_maximum = optional_htlc_maximum_to_u64(&latest_update.htlc_maximum_msat);
				new_htlc_maximum.write(&mut delta_serialization).unwrap();
			}
		}
	}
	let scid_delta = BigSize(latest_update.short_channel_id - previous_scid);
	scid_delta.write(&mut prefixed_serialization);

	serialized_flags.write(&mut prefixed_serialization);
	prefixed_serialization.append(&mut delta_serialization);

	prefixed_serialization
}

pub(super) fn find_most_common_histogram_entry_with_default<T: Copy>(histogram: HashMap<T, usize>, default: T) -> T {
	let most_frequent_entry = histogram.iter().max_by(|a, b| a.1.cmp(&b.1));
	if let Some(entry_details) = most_frequent_entry {
		// .0 is the value
		// .1 is the frequency
		return entry_details.0.to_owned();
	}
	// the default should pretty much always be a 0 as T
	// though for htlc maximum msat it could be a u64::max
	default
}

pub(super) fn optional_htlc_maximum_to_u64(htlc_maximum_msat: &OptionalField<u64>) -> u64 {
	if let OptionalField::Present(maximum) = htlc_maximum_msat {
		maximum.clone()
	} else {
		u64::MAX
	}
}

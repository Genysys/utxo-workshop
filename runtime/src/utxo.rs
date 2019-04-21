/// A reimplementation of Dima's UTXO chain
use support::{
    decl_module, 
    decl_storage, 
    decl_event, 
    StorageValue,
    StorageMap,
    ensure,
    dispatch::Result
};

use system::ensure_inherent;
use primitives::{H256, H512};
use rstd::collections::btree_map::BTreeMap;
use runtime_primitives::traits::{As, Hash, BlakeTwo256};
use runtime_primitives::{Serialize, Deserialize};
use runtime_io::{ed25519_verify};
use parity_codec::{Encode, Decode};
use super::Consensus;

pub trait Trait: system::Trait {
	type Event: From<Event> + Into<<Self as system::Trait>::Event>;
}

#[cfg_attr(feature = "std", derive(Serialize, Deserialize, Debug))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Default, Clone, Encode, Decode, Hash)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>
}

type Signature = H512; 

#[cfg_attr(feature = "std", derive(Serialize, Deserialize, Debug))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Default, Clone, Encode, Decode, Hash)]
pub struct TransactionInput {
    // Referen  ce to the input value
    pub parent_output: H256,  // referred UTXO
    pub signature: Signature, // proof that owner is authorized to spend referred UTXO
    // omitted traits ord, partialord bc its not implemented for signature yet
}

pub type Value = u128; // Alias u128 to Value

#[cfg_attr(feature = "std", derive(Serialize, Deserialize, Debug))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Default, Clone, Encode, Decode, Hash)]
pub struct TransactionOutput {
    pub value: Value,
    pub pubkey: H256, // pub key of the output, owner has to have private key
    pub salt: u64,    // distinguishes outputs of same value/pubkey apart
}

// A UTXO can be locked (indefinitely) or until a certain block height
#[cfg_attr(feature = "std", derive(Serialize, Deserialize, Debug))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Encode, Decode, Hash)]
pub enum LockStatus<BlockNumber> {
    Locked,
    LockedUntil(BlockNumber),
}

decl_storage! {
	trait Store for Module<T: Trait> as Utxo {

        UnspentOutputs build(|config: &GenesisConfig<T>| {
			config.initial_utxo
				.iter()
				.cloned()
				.map(|u| (BlakeTwo256::hash_of(&u), u))
				.collect::<Vec<_>>()
		}): map H256 => Option<TransactionOutput>;

        pub DustTotal get(leftover_total): Value;

        // A map of outputs that are locked
        LockedOutputs: map H256 => Option<LockStatus<T::BlockNumber>>; 
	}

    add_extra_genesis {
		config(initial_utxo): Vec<TransactionOutput>;
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        fn deposit_event() = default;

        // custom function for minting tokens (instead of doing in genesis config)
        fn mint(origin, value: Value, pubkey: H256) -> Result {
            ensure_inherent(origin)?;
            let salt:u64 = <system::Module<T>>::block_number().as_();
            let utxo = TransactionOutput { value, pubkey, salt };
            let hash = BlakeTwo256::hash_of(&utxo); 

            if !<UnspentOutputs<T>>::exists(hash) {
                <UnspentOutputs<T>>::insert(hash, utxo);
            } else {
                runtime_io::print("cannot mint due to hash collision");
            }
            
            Ok(())
        }

        fn execute(origin, transaction: Transaction) -> Result {
            ensure_inherent(origin)?;

            // Verify the transaction
            let dust = match Self::verify_transaction(&transaction)? {
                CheckInfo::Totals{input, output} => input - output,
                CheckInfo::MissingInputs(_) => return Err("Invalid transaction inputs")
            };

            // Update unspent outputs
            Self::_update_storage(&transaction, dust)?;
            
            Ok(())
        }

        fn on_finalize() {
            //                                                        Q: this ok?
            let auth:Vec<_> = Consensus::authorities().iter().map(|x| x.0.into() ).collect();
                                    //Vec<T::SessionKey>
            Self::_spend_dust(&auth);
        }
	}
}

decl_event!(
	pub enum Event {
		TransactionExecuted(Transaction),
	}
);
// nice coding pattern, everytime you return a value, 1. wrap enum in resultType 2. use enum to represent different outcomes

pub enum CheckInfo<'a> {
    Totals { input: Value, output: Value },   // struct
    MissingInputs(Vec<&'a H256>),     //Q: why is there a lifetime/reference here?
}

pub type CheckResult<'a> = std::result::Result<CheckInfo<'a>, &'static str>; // errors are already defined


impl<T: Trait> Module<T> {
    // TODO take this out into a trait
    pub fn _lock_utxo(hash: &H256, until: Option<T::BlockNumber>) -> Result {
        ensure!(!<LockedOutputs<T>>::exists(hash), "utxo is already locked");
		ensure!(<UnspentOutputs<T>>::exists(hash), "utxo does not exist");

        if let Some(until) = until {
            ensure!(until > <system::Module<T>>::block_number(), "block number is in the past");
            <LockedOutputs<T>>::insert(hash, LockStatus::LockedUntil(until));    
        } else {
            <LockedOutputs<T>>::insert(hash, LockStatus::Locked);    
        }
        
        Ok(())
    }

    pub fn _unlock_utxo(hash: &H256) -> Result {
        ensure!(!<LockedOutputs<T>>::exists(hash), "utxo is not locked");
        <LockedOutputs<T>>::remove(hash);
        Ok(())
    }
    
    //                  You almost always want &[T] over &Vec<T>
    fn _spend_dust(authorities: &[H256]) { //TODO double check
        let dust = <DustTotal<T>>::take();
        let dust_per_authority: Value = dust.checked_div(authorities.len() as Value).ok_or("No authorities").unwrap();
        if dust_per_authority == 0 { return };
        
        // Q: Should we save the remainder here?
        let dust_remainder = dust.checked_sub(dust_per_authority * authorities.len() as Value).ok_or("Sub underflow").unwrap();
        <DustTotal<T>>::put(dust_remainder as Value);
        
        for authority in authorities {
            let utxo = TransactionOutput {
                value: dust_per_authority,
                pubkey: *authority,
                salt: <system::Module<T>>::block_number().as_(),
            };
            
            let hash = BlakeTwo256::hash_of(&utxo);
            
            if !<UnspentOutputs<T>>::exists(hash) {
                <UnspentOutputs<T>>::insert(hash, utxo);
                runtime_io::print("leftover share sent to");
				runtime_io::print(hash.as_fixed_bytes() as &[u8]);
            } else {
                runtime_io::print("leftover share wasted due to hash collision");
            }
        }
    }

    fn _update_storage(transaction: &Transaction, dust: Value) -> Result {
        // update dust
        let dust_total = <DustTotal<T>>::get().checked_add(dust).ok_or("Dust overflow")?;
        <DustTotal<T>>::put(dust_total);
        
        // update unspent outputs
        for input in &transaction.inputs {
            <UnspentOutputs<T>>::remove(input.parent_output);
        }

        for output in &transaction.outputs {
            let hash = BlakeTwo256::hash_of(output); 
            <UnspentOutputs<T>>::insert(hash, output);
        }

        Ok(())
    }

    /// Verifies the transaction validity, returns the outcome
    pub fn verify_transaction(transaction: &Transaction) -> CheckResult<'_> {
        
        // TODO

        Ok(CheckInfo::Totals { input: 0, output: 0 })
        
        
    }

}

/// tests for this module
#[cfg(test)]
mod tests {
	use super::*;

	use runtime_io::with_externalities;
	use primitives::{H256, Blake2Hasher};
	use support::{
        impl_outer_origin, 
        assert_ok,
        assert_err,
    };
	use runtime_primitives::{
		BuildStorage,
		traits::{BlakeTwo256, IdentityLookup},
		testing::{Digest, DigestItem, Header}
	};

	impl_outer_origin! {
		pub enum Origin for Test {}
	}

	#[derive(Clone, Eq, PartialEq)]
	pub struct Test;
	impl system::Trait for Test {
		type Origin = Origin;
		type Index = u64;
		type BlockNumber = u64;
		type Hash = H256;
		type Hashing = BlakeTwo256;
		type Digest = Digest;
		type AccountId = u64;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type Event = ();
		type Log = DigestItem;
	}
	impl Trait for Test {
		type Event = ();
	}

	type Utxo = Module<Test>;
    
    // Test set up
    // Alice's Public Key: from_legacy_string("Alice", Some("recover"));
    const ALICE_KEY: [u8; 32] = [209, 114, 167, 76, 218, 76, 134, 89, 18, 195, 43, 160, 168, 10, 87, 174, 105, 171, 174, 65, 14, 92, 203, 89, 222, 232, 78, 47, 68, 50, 219, 79];
    
    // Alice's Signature to spend alice_utxo(): signs a token she owns Pair::sign(&message[..])
    const ALICE_SIG: [u8; 64] = [203, 25, 139, 36, 34, 10, 235, 226, 189, 110, 216, 143, 155, 17, 148, 6, 191, 239, 29, 227, 118, 59, 125, 216, 222, 242, 222, 49, 68, 49, 41, 242, 128, 133, 202, 59, 127, 159, 239, 139, 18, 88, 255, 236, 155, 254, 40, 185, 42, 96, 60, 156, 203, 11, 101, 239, 228, 218, 62, 202, 205, 17, 41, 7];

    // Alice's Signature to spend alice_utxo_100(): signs a token she owns Pair::sign(&message[..])
    const ALICE_SIG100: [u8; 64] = [37, 190, 14, 182, 163, 218, 61, 32, 245, 202, 94, 196, 186, 129, 171, 128, 91, 163, 51, 30, 146, 219, 237, 78, 145, 75, 195, 175, 212, 99, 230, 232, 234, 49, 208, 115, 146, 75, 228, 253, 244, 238, 116, 198, 138, 15, 111, 214, 243, 157, 62, 146, 122, 211, 217, 74, 27, 193, 223, 79, 114, 173, 233, 1];

    // Creates a max value UTXO for Alice
    fn alice_utxo() -> (H256, TransactionOutput) {
		let transaction = TransactionOutput {
			value: Value::max_value(),
			pubkey: H256::from_slice(&ALICE_KEY),
			salt: 0,
		};

		(BlakeTwo256::hash_of(&transaction), transaction)
	}

    // Creates a 100 value UTXO for Alice
    fn alice_utxo_100() -> (H256, TransactionOutput) {
		let transaction = TransactionOutput {
			value: 100,
			pubkey: H256::from_slice(&ALICE_KEY),
			salt: 0,
		};

		(BlakeTwo256::hash_of(&transaction), transaction)
	}

	// This function basically just builds a genesis storage key/value store according to
	// our desired mockup.
	fn new_test_ext() -> runtime_io::TestExternalities<Blake2Hasher> {
		let mut t = system::GenesisConfig::<Test>::default().build_storage().unwrap().0;
        t.extend(GenesisConfig::<Test>{
            initial_utxo: vec![alice_utxo().1, alice_utxo_100().1],
            ..Default::default()
        }.build_storage().unwrap().0);
        t.into()
	}

    // Exercise 1: Fortify transactions against attacks
    // ================================================
    // 
    // The following tests simulate malicious UTXO transactions
    // Implement the verify_transaction() function to thwart such attacks
    // 
    // Hint: Examine types CheckResult, CheckInfo for the expected behaviors of this function
    // Hint: Make this function public, as it will be later used outside of this module

    #[test]
	fn attack_with_empty_transactions() {
		with_externalities(&mut new_test_ext(), || {
			assert_err!(
				Utxo::execute(Origin::INHERENT, Transaction::default()), // an empty trx
				"no inputs"
			);

			assert_err!(
				Utxo::execute(
					Origin::INHERENT,
					Transaction {
						inputs: vec![TransactionInput::default()], // an empty trx
						outputs: vec![],
					}
				),
				"no outputs"
			);
		});
	}

    #[test]
    fn attack_by_double_counting_input() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo();

            let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG),
					}, 
                    TransactionInput {
						parent_output: parent_hash,     // Double spending input!
						signature: Signature::from_slice(&ALICE_SIG),
					}

				],
				outputs: vec![
					TransactionOutput {
						value: 100,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 0,
					}
				],
			};

            assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "each input must only be used once"
            );
        });
    }

    #[test]
    fn attack_by_double_generating_output() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo();

            let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG),
					}
				],
				outputs: vec![
					TransactionOutput {
						value: 100,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 0,
					},
                    TransactionOutput {         // Same output defined here!
						value: 100,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 0,
					}
				],
			};

            assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "each output must be defined only once"
            );
        });
    }

    #[test]
    fn attack_with_invalid_signature() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo();
            
			let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: H512::random(),  // Just a random signature!
					}
				],
				outputs: vec![
					TransactionOutput {
						value: 100,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 0,
					}
				],
			};

			assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "signature must be valid"
            );
        });
    }

    #[test]
    fn attack_by_permanently_sinking_outputs() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo();

            let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG),
					}
				],
				outputs: vec![
					TransactionOutput {
						value: 0,               // A 0 value output burns this output forever!
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 0,
					}
				],
			};

            assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "output value must be nonzero"
            );
        });
    }

    #[test]
    fn attack_by_overflowing() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo();

            let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG),
					}
				],
				outputs: vec![
					TransactionOutput {
						value: Value::max_value(),
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 1,
					},
                    TransactionOutput {
						value: 10 as Value, // Attempts to do overflow total output value
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 1,
					}
				],
			};

            assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "output value overflow"
            );
        });
    }

    #[test]
    fn attack_by_over_spending() {
        with_externalities(&mut new_test_ext(), || {
            let (parent_hash, _) = alice_utxo_100();

            let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG100),
					}
				],
				outputs: vec![
					TransactionOutput {
						value: 100 as Value,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 1,
					},
                    TransactionOutput {
						value: 1 as Value,  // Creates 1 new utxo out of thin air!
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 1,
					}
				],
			};

            assert_err!(Utxo::execute(Origin::INHERENT, transaction), 
                "output value must not exceed input value"
            );
        });
    }

	#[test]
    fn valid_transaction() {
		with_externalities(&mut new_test_ext(), || {
			let (parent_hash, _) = alice_utxo();
            
			let transaction = Transaction {
				inputs: vec![
					TransactionInput {
						parent_output: parent_hash,
						signature: Signature::from_slice(&ALICE_SIG),
					}
				],
				outputs: vec![
					TransactionOutput {
						value: 100,
						pubkey: H256::from_slice(&ALICE_KEY),
						salt: 2,
					}
				],
			};

			let output_hash = BlakeTwo256::hash_of(&transaction.outputs[0]);

			assert_ok!(Utxo::execute(Origin::INHERENT, transaction));
			assert!(!<UnspentOutputs<Test>>::exists(parent_hash));
			assert!(<UnspentOutputs<Test>>::exists(output_hash));
		});
	}
}


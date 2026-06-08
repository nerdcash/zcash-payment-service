use zcash_client_backend::scanning::ScanningKeys;
use zcash_keys::{
    address::UnifiedAddress,
    encoding::AddressCodec,
    keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedIncomingViewingKey},
};
use zcash_protocol::consensus::Network;
use zip32::DiversifierIndex;
use zip32::Scope;

use crate::error::AppError;

pub type CanonicalScanAccountId = u32;
pub const CANONICAL_SCAN_ACCOUNT_ID: CanonicalScanAccountId = 0;

pub struct DerivedAddress {
    pub encoded: String,
    pub diversifier_index_be: Vec<u8>,
    pub orchard_receiver: Vec<u8>,
    pub sapling_receiver: Vec<u8>,
}

#[derive(Clone)]
pub struct WalletView {
    encoded_uivk: String,
    uivk: UnifiedIncomingViewingKey,
    network: Network,
}

impl WalletView {
    pub fn decode(network_name: &str, encoded_uivk: &str) -> Result<Self, AppError> {
        let network = consensus_network(network_name)?;
        let uivk =
            UnifiedIncomingViewingKey::decode(&network, encoded_uivk).map_err(AppError::Wallet)?;

        if !uivk.has_orchard() || !uivk.has_sapling() {
            return Err(AppError::Wallet(
                "configured UIVK must contain Orchard and Sapling receivers".into(),
            ));
        }

        Ok(Self {
            encoded_uivk: encoded_uivk.to_string(),
            uivk,
            network,
        })
    }

    pub fn encoded_uivk(&self) -> &str {
        &self.encoded_uivk
    }

    pub fn is_valid_issued_address_shape(&self, encoded_address: &str) -> bool {
        match UnifiedAddress::decode(&self.network, encoded_address) {
            Ok(address) => {
                address.has_orchard()
                    && address.has_sapling()
                    && !address.has_transparent()
                    && address.unknown().is_empty()
            }
            Err(_) => false,
        }
    }

    pub fn is_issued_address_for_wallet(&self, encoded_address: &str) -> bool {
        let request = UnifiedAddressRequest::unsafe_custom(
            ReceiverRequirement::Require,
            ReceiverRequirement::Require,
            ReceiverRequirement::Omit,
        );

        let Ok(address) = UnifiedAddress::decode(&self.network, encoded_address) else {
            return false;
        };
        if !address.has_orchard()
            || !address.has_sapling()
            || address.has_transparent()
            || !address.unknown().is_empty()
        {
            return false;
        }

        let Some(orchard_address) = address.orchard() else {
            return false;
        };
        let Some(orchard_ivk) = self.uivk.orchard().as_ref() else {
            return false;
        };
        let Some(index) = orchard_ivk.diversifier_index(orchard_address) else {
            return false;
        };

        self.uivk
            .address(index, request)
            .map(|derived| derived.encode(&self.network) == encoded_address)
            .unwrap_or(false)
    }

    pub fn derive_address_after(
        &self,
        last_diversifier_index_be: Option<&[u8]>,
    ) -> Result<DerivedAddress, AppError> {
        let mut start = diversifier_from_bytes(last_diversifier_index_be)?;
        if last_diversifier_index_be.is_some() {
            start
                .increment()
                .map_err(|_| AppError::Wallet("diversifier space exhausted".into()))?;
        }

        let request = UnifiedAddressRequest::unsafe_custom(
            ReceiverRequirement::Require,
            ReceiverRequirement::Require,
            ReceiverRequirement::Omit,
        );
        let (address, index) = self
            .uivk
            .find_address(start, request)
            .map_err(|error| AppError::Wallet(error.to_string()))?;

        let orchard_receiver = address
            .orchard()
            .ok_or_else(|| AppError::Wallet("derived address missing Orchard receiver".into()))?
            .to_raw_address_bytes()
            .to_vec();
        let sapling_receiver = address
            .sapling()
            .ok_or_else(|| AppError::Wallet("derived address missing Sapling receiver".into()))?
            .to_bytes()
            .to_vec();

        Ok(DerivedAddress {
            encoded: address.encode(&self.network),
            diversifier_index_be: index.as_bytes().to_vec(),
            orchard_receiver,
            sapling_receiver,
        })
    }
}

pub fn scanning_keys_from_uivk(
    network_name: &str,
    encoded_uivk: &str,
) -> Result<ScanningKeys<CanonicalScanAccountId, (CanonicalScanAccountId, Scope)>, AppError> {
    use incrementalmerkletree::Position;
    use sapling::note_encryption::SaplingDomain;
    use zcash_client_backend::scanning::ScanningKeyOps;

    let network = consensus_network(network_name)?;
    let uivk =
        UnifiedIncomingViewingKey::decode(&network, encoded_uivk).map_err(AppError::Wallet)?;

    if !uivk.has_orchard() || !uivk.has_sapling() {
        return Err(AppError::Wallet(
            "configured UIVK must contain Orchard and Sapling receivers".into(),
        ));
    }

    struct SaplingScanKey {
        ivk: sapling::zip32::IncomingViewingKey,
        account_id: CanonicalScanAccountId,
    }

    impl ScanningKeyOps<SaplingDomain, CanonicalScanAccountId, sapling::Nullifier> for SaplingScanKey {
        fn prepare(&self) -> sapling::note_encryption::PreparedIncomingViewingKey {
            self.ivk.prepare()
        }
        fn nf(
            &self,
            _note: &sapling::Note,
            _note_position: Position,
        ) -> Option<sapling::Nullifier> {
            None
        }
        fn account_id(&self) -> &CanonicalScanAccountId {
            &self.account_id
        }
        fn key_scope(&self) -> Option<Scope> {
            Some(Scope::External)
        }
    }

    struct OrchardScanKey {
        ivk: orchard::keys::IncomingViewingKey,
        account_id: CanonicalScanAccountId,
    }

    impl
        ScanningKeyOps<
            orchard::note_encryption::OrchardDomain,
            CanonicalScanAccountId,
            orchard::note::Nullifier,
        > for OrchardScanKey
    {
        fn prepare(&self) -> orchard::keys::PreparedIncomingViewingKey {
            self.ivk.prepare()
        }
        fn nf(
            &self,
            _note: &orchard::Note,
            _note_position: Position,
        ) -> Option<orchard::note::Nullifier> {
            None
        }
        fn account_id(&self) -> &CanonicalScanAccountId {
            &self.account_id
        }
        fn key_scope(&self) -> Option<Scope> {
            Some(Scope::External)
        }
    }

    #[allow(clippy::type_complexity)]
    let mut sapling_keys: std::collections::HashMap<
        (CanonicalScanAccountId, Scope),
        Box<
            dyn ScanningKeyOps<SaplingDomain, CanonicalScanAccountId, sapling::Nullifier>
                + Send
                + Sync,
        >,
    > = std::collections::HashMap::new();

    #[allow(clippy::type_complexity)]
    let mut orchard_keys: std::collections::HashMap<
        (CanonicalScanAccountId, Scope),
        Box<
            dyn ScanningKeyOps<
                    orchard::note_encryption::OrchardDomain,
                    CanonicalScanAccountId,
                    orchard::note::Nullifier,
                > + Send
                + Sync,
        >,
    > = std::collections::HashMap::new();

    if let Some(sapling_ivk) = uivk.sapling().as_ref() {
        sapling_keys.insert(
            (CANONICAL_SCAN_ACCOUNT_ID, Scope::External),
            Box::new(SaplingScanKey {
                ivk: sapling_ivk.clone(),
                account_id: CANONICAL_SCAN_ACCOUNT_ID,
            }),
        );
    }

    if let Some(orchard_ivk) = uivk.orchard().as_ref() {
        orchard_keys.insert(
            (CANONICAL_SCAN_ACCOUNT_ID, Scope::External),
            Box::new(OrchardScanKey {
                ivk: orchard_ivk.clone(),
                account_id: CANONICAL_SCAN_ACCOUNT_ID,
            }),
        );
    }

    Ok(ScanningKeys::new(sapling_keys, orchard_keys))
}

fn diversifier_from_bytes(bytes: Option<&[u8]>) -> Result<DiversifierIndex, AppError> {
    match bytes {
        Some(bytes) => {
            let array: [u8; 11] = bytes.try_into().map_err(|_| {
                AppError::Wallet("stored diversifier index must be exactly 11 bytes".into())
            })?;
            Ok(DiversifierIndex::from(array))
        }
        None => Ok(DiversifierIndex::new()),
    }
}

pub fn consensus_network(network_name: &str) -> Result<Network, AppError> {
    match network_name {
        "mainnet" => Ok(Network::MainNetwork),
        "testnet" => Ok(Network::TestNetwork),
        other => Err(AppError::InvalidConfig(format!(
            "unsupported Zcash network '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::WalletView;

    const MAINNET_UIVK_NO_TRANSPARENT: &str = "uivk1020vq9j5zeqxh303sxa0zv2hn9wm9fev8x0p8yqxdwyzde9r4c90fcglc63usj0ycl2scy8zxuhtser0qrq356xfy8x3vyuxu7f6gas75svl9v9m3ctuazsu0ar8e8crtx7x6zgh4kw8xm3q4rlkpm9er2wefxhhf9pn547gpuz9vw27gsdp6c03nwlrxgzhr2g6xek0x8l5avrx9ue9lf032tr7kmhqf3nfdxg7ldfgx6yf09g";
    const TESTNET_UFVK_WITH_OPTIONAL_TRANSPARENT: &str = "uviewtest1tcygrtut692vqlx9nlyknx0fqq59am5vhaf97gxncqnfn87qrgey68777tumstc2lcp4r9yxd3fknkpmxtgw8awhcg40cw00ahvtaeqmpfqvjz6e3v234zsfvdvt6dm8dpzxv970wkdv2jrfm3t2m9cde9ry8mrxr286ns4yqwmcx3k4netqqhgldthnhzhlpg0lk00eruy4tf3fx3k9xn7fywppj8wyzjjc3dcrqe6kxnc6zxpfly9e2uk3k7jyy3n70zpj5zfheedzz0sw2pp96rvy9xt2dw94nplfx0usrwtshrmf5xwq84qcq459kvks5g28gvkxrjpujgc9gkjt5np5m4afruk0z8zlyd65hfqqu9pg3u9lkk26r7ad4l59yy3tn2xlmad42a6kee8l92ddj7tgf4fhv4x9thx7kf28jc6gvf3xr3lhtegd8ly590595g6dfh3w7nalmt8zx6yestgfqu9uvg8gkwkwmuc4u5jsecqu";

    #[test]
    fn constructs_scanning_keys_from_uivk() {
        let scanning_keys =
            super::scanning_keys_from_uivk("mainnet", MAINNET_UIVK_NO_TRANSPARENT).unwrap();
        assert_eq!(scanning_keys.sapling().len(), 1);
        assert_eq!(scanning_keys.orchard().len(), 1);
    }

    #[test]
    fn decodes_mainnet_uivk_without_transparent_receiver() {
        let wallet = WalletView::decode("mainnet", MAINNET_UIVK_NO_TRANSPARENT).unwrap();
        assert_eq!(wallet.encoded_uivk(), MAINNET_UIVK_NO_TRANSPARENT);
    }

    #[test]
    fn derives_two_shielded_receiver_address() {
        let wallet = WalletView::decode("mainnet", MAINNET_UIVK_NO_TRANSPARENT).unwrap();
        let derived = wallet.derive_address_after(None).unwrap();
        assert!(wallet.is_valid_issued_address_shape(&derived.encoded));
        assert!(wallet.is_issued_address_for_wallet(&derived.encoded));
        assert_eq!(derived.diversifier_index_be.len(), 11);
        assert!(!derived.orchard_receiver.is_empty());
        assert!(!derived.sapling_receiver.is_empty());
    }

    #[test]
    fn rejects_shielded_address_from_other_wallet_identity() {
        let mainnet_wallet = WalletView::decode("mainnet", MAINNET_UIVK_NO_TRANSPARENT).unwrap();
        let testnet_uivk = {
            use zcash_keys::keys::UnifiedFullViewingKey;
            let network = super::consensus_network("testnet").unwrap();
            let ufvk =
                UnifiedFullViewingKey::decode(&network, TESTNET_UFVK_WITH_OPTIONAL_TRANSPARENT)
                    .unwrap();
            ufvk.to_unified_incoming_viewing_key().encode(&network)
        };
        let testnet_wallet = WalletView::decode("testnet", &testnet_uivk).unwrap();
        let testnet_address = testnet_wallet.derive_address_after(None).unwrap();

        assert!(!mainnet_wallet.is_issued_address_for_wallet(&testnet_address.encoded));
    }

    #[test]
    fn accepts_uivk_that_was_derived_from_ufvk_with_transparent_capability() {
        let encoded_uivk = {
            use zcash_keys::keys::UnifiedFullViewingKey;
            let network = super::consensus_network("testnet").unwrap();
            let ufvk =
                UnifiedFullViewingKey::decode(&network, TESTNET_UFVK_WITH_OPTIONAL_TRANSPARENT)
                    .unwrap();
            ufvk.to_unified_incoming_viewing_key().encode(&network)
        };
        let scanning_keys = super::scanning_keys_from_uivk("testnet", &encoded_uivk).unwrap();
        let wallet = WalletView::decode("testnet", &encoded_uivk).unwrap();

        assert_eq!(wallet.encoded_uivk(), encoded_uivk);
        assert_eq!(scanning_keys.sapling().len(), 1);
        assert_eq!(scanning_keys.orchard().len(), 1);
    }

    /// Verify that the sapling IVK bytes from UIVK match those from UFVK so scanning is equivalent.
    #[test]
    fn uivk_sapling_ivk_matches_ufvk_external_ivk() {
        use zcash_keys::keys::UnifiedFullViewingKey;
        let network = super::consensus_network("testnet").unwrap();
        let ufvk = UnifiedFullViewingKey::decode(&network, TESTNET_UFVK_WITH_OPTIONAL_TRANSPARENT)
            .unwrap();

        // Get sapling IVK bytes from UFVK path (the old scanning approach).
        let dfvk = ufvk.sapling().unwrap();
        let ufvk_external_ivk_bytes = dfvk.to_external_ivk().to_bytes();

        // Get sapling IVK bytes from UIVK path (the new scanning approach).
        let uivk = ufvk.to_unified_incoming_viewing_key();
        let uivk_sapling_ivk_bytes = uivk.sapling().as_ref().unwrap().to_bytes();

        // The raw 64-byte encodings must match (dk || ivk).
        assert_eq!(
            ufvk_external_ivk_bytes, uivk_sapling_ivk_bytes,
            "UIVK sapling IVK bytes must equal UFVK external IVK bytes"
        );
    }
}

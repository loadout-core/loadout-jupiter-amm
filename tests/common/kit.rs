//! A label-only adapter wrapper selects each executable in Jupiter's unmodified test kit.
use jupiter_amm_interface::*;
use loadout_jupiter_amm::LoadoutCurve;
use solana_pubkey::Pubkey;

#[derive(Clone)]
pub struct FixtureAmm<const PREVIEW: bool>(LoadoutCurve);
impl<const PREVIEW: bool> Amm for FixtureAmm<PREVIEW> {
    fn from_keyed_account(keyed: &KeyedAccount, context: &AmmContext) -> Result<Self, AmmError> {
        LoadoutCurve::from_keyed_account(keyed, context).map(Self)
    }
    fn label(&self) -> AmmLabel {
        if PREVIEW { "next" } else { "nominated" }
    }
    fn program_id(&self) -> Pubkey {
        self.0.program_id()
    }
    fn key(&self) -> Pubkey {
        self.0.key()
    }
    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        self.0.get_reserve_mints()
    }
    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        self.0.get_accounts_to_update()
    }
    fn update(&mut self, provider: impl AccountProvider) -> Result<(), AmmError> {
        self.0.update(provider)
    }
    fn quote(&self, params: &QuoteParams) -> Result<Quote, AmmError> {
        self.0.quote(params)
    }
    fn get_swap_and_account_metas(
        &self,
        params: &SwapParams,
    ) -> Result<SwapAndAccountMetas, AmmError> {
        self.0.get_swap_and_account_metas(params)
    }
    fn get_accounts_len(&self) -> usize {
        self.0.get_accounts_len()
    }
    fn is_active(&self) -> bool {
        self.0.is_active()
    }
}

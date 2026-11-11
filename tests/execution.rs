//! Core/Solana composition constraints only. The relay is not Jupiter's implementation.
mod common;
#[path = "common/execution.rs"]
mod lab;
use common::{Snapshot, amm_at};
use lab::*;
use loadout_jupiter_amm::{NATIVE_MINT, protocol_fee_shard, trader_accounts};
use solana_signer::Signer;
const ACTIVE: &str = "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn";
const NEAR: &str = "9pN9gVHZXHYoPdW44i7RCS14kBcQdsFGqh3NL1F6tTvX";
const PROTECTED: &str = "5o56bndGq6YFZFwCTuCmZv42g89bi6gdhC2TEeRYaFbo";
const SOL: u64 = 1_000_000_000;
fn quote(lab: &Lab, buy: bool, amount: u64) -> jupiter_amm_interface::Quote {
    let s = lab.snapshot();
    let token = amm_at(&s, None).0.accounts().token_mint;
    common::quote(
        &s,
        if buy { NATIVE_MINT } else { token },
        if buy { token } else { NATIVE_MINT },
        amount,
        None,
    )
    .unwrap()
}
#[test]
fn cpi_wallet_and_pda_authorities_buy_to_a_distinct_users_token_account() {
    for pda in [false, true] {
        let mut lab = Lab::new(Snapshot::load(ACTIVE), pda);
        let q = quote(&lab, true, SOL / 10);
        let before = lab.balance(lab.authority);
        let user_before = lab.balance(lab.user.pubkey());
        let ix = lab.relay(lab.swap(true, SOL / 10, q.out_amount, lab.user_token), pda);
        let sent = lab.send(&[ix]);
        assert!(sent.error.is_none(), "{sent:?}");
        assert!(
            sent.logs.iter().any(|l| l.contains("invoke [2]")),
            "real CPI depth"
        );
        assert_eq!(lab.tokens(lab.user_token), q.out_amount);
        assert_eq!(event_trader(&sent), lab.authority);
        assert_eq!(
            before - lab.balance(lab.authority),
            q.in_amount + lab.rent()
        );
        assert_eq!(lab.balance(lab.user.pubkey()), user_before);
        assert!(sent.transaction_fee > 0);
        assert!(
            sent.logs.iter().any(|l| l.starts_with("Program data: ")),
            "SwapEvent emitted"
        );
        for key in [
            trader_accounts(&lab.user.pubkey()).0,
            trader_accounts(&lab.user.pubkey()).1,
        ] {
            assert_eq!(lab.balance(key), 0);
        }
    }
}

#[test]
fn delegated_sells_pay_native_sol_and_attribute_the_event_to_authority() {
    for pda in [false, true] {
        let mut lab = Lab::new(Snapshot::load(ACTIVE), pda);
        // Obtain the sell input through an actual CPI buy, not synthetic token funding.
        let bought = quote(&lab, true, SOL / 10);
        let buy = lab.send(&[lab.relay(
            lab.swap(true, SOL / 10, bought.out_amount, lab.user_token),
            pda,
        )]);
        assert!(buy.error.is_none(), "{buy:?}");
        let amount = bought.out_amount / 2;
        let q = quote(&lab, false, amount);
        let bad = lab.relay(lab.swap(false, amount, q.out_amount, lab.user_token), pda);
        let rejected = lab.send(&[bad]);
        assert!(
            rejected.error.as_deref().unwrap().contains("Custom(4)"),
            "{rejected:?}"
        );
        let approved = lab.send(&[approve(
            lab.user_token,
            lab.authority,
            lab.user.pubkey(),
            amount,
        )]);
        assert!(approved.error.is_none(), "{approved:?}");
        let authority_before = lab.balance(lab.authority);
        let rent_before = lab.rent();
        let user_before = lab.balance(lab.user.pubkey());
        let sent =
            lab.send(&[lab.relay(lab.swap(false, amount, q.out_amount, lab.user_token), pda)]);
        assert!(sent.error.is_none(), "{sent:?}");
        assert_eq!(lab.tokens(lab.user_token), bought.out_amount - amount);
        assert_eq!(
            i128::from(lab.balance(lab.authority)) - i128::from(authority_before),
            i128::from(q.out_amount) - i128::from(lab.rent() - rent_before)
        );
        assert_eq!(lab.balance(lab.user.pubkey()), user_before);
        assert_eq!(event_trader(&sent), lab.authority);
    }
}

#[test]
fn cpi_rejects_missing_signer_and_writable_privileges_without_state_changes() {
    for index in [12, 2] {
        let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
        let mut ix = lab.relay(lab.swap(true, SOL / 10, 0, lab.user_token), false);
        if index == 12 {
            ix.accounts[index].is_signer = false;
        } else {
            ix.accounts[index].is_writable = false;
        }
        let sent = lab.send(&[ix]);
        assert!(
            sent.error
                .as_deref()
                .unwrap()
                .contains("PrivilegeEscalation"),
            "{sent:?}"
        );
        assert_eq!(lab.rent(), 0);
    }
}

#[test]
fn data_carrying_native_sol_authority_cannot_fund_first_trade() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), true);
    lab.set_authority_data(); // Synthetic invalid input; never a fixture rewrite.
    let sent = lab.send(&[lab.relay(lab.swap(true, SOL / 10, 0, lab.user_token), true)]);
    assert!(sent.error.is_some(), "{sent:?}");
    assert!(
        sent.logs.iter().any(|l| l.contains("must not carry data")),
        "{sent:?}"
    );
    assert_eq!(lab.rent(), 0);
}

#[test]
fn an_unfunded_fee_shard_is_an_execution_prerequisite_not_quote_parity() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let key = protocol_fee_shard(&lab.authority);
    let mut shard = lab.svm.get_account(&key).unwrap();
    shard.lamports = 0;
    lab.svm.set_account(key, shard).unwrap();
    let q = quote(&lab, true, SOL / 10); // Quote reads only curve/project/FeeConfig.
    let sent = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, q.out_amount, lab.user_token),
        false,
    )]);
    assert!(
        sent.error.as_deref().unwrap().contains("Custom(12171)"),
        "{sent:?}"
    );
}

#[test]
fn capped_buy_preserves_existing_wsol_and_explicitly_returns_surplus_and_rent() {
    let mut lab = Lab::new(Snapshot::load(NEAR), false);
    let offered = 100 * SOL;
    let unrelated = 3 * SOL;
    let source = lab.wsol(lab.authority);
    let temp = lab.wsol(lab.user.pubkey());
    let funded = lab.send(&[
        transfer(lab.authority, source, offered + unrelated),
        sync(source),
    ]);
    assert!(funded.error.is_none(), "{funded:?}");
    let source_rent = lab.balance(source) - lab.tokens(source);
    let temp_rent = lab.balance(temp);
    let q = quote(&lab, true, offered);
    assert!(q.in_amount < offered);
    let before_user = lab.balance(lab.user.pubkey());
    let before_authority = lab.balance(lab.authority);
    let before_payer = lab.balance(lab.payer.pubkey());
    let sent = lab.send(&[
        token_transfer(source, temp, lab.authority, offered),
        close(temp, lab.authority, lab.user.pubkey()),
        lab.relay(lab.swap(true, offered, q.out_amount, lab.user_token), false),
        // This explicit refund policy is a test assumption, not Jupiter's convention.
        transfer(lab.authority, lab.user.pubkey(), offered - q.in_amount),
        transfer(lab.authority, lab.payer.pubkey(), temp_rent),
    ]);
    assert!(sent.error.is_none(), "{sent:?}");
    assert_eq!(lab.tokens(source), unrelated, "existing WSOL was not swept");
    assert_eq!(lab.balance(source), unrelated + source_rent);
    assert_eq!(lab.balance(temp), 0);
    assert_eq!(lab.tokens(lab.user_token), q.out_amount);
    assert_eq!(
        lab.balance(lab.user.pubkey()) - before_user,
        offered - q.in_amount
    );
    assert_eq!(before_authority - lab.balance(lab.authority), lab.rent());
    assert_eq!(
        i128::from(lab.balance(lab.payer.pubkey())) - i128::from(before_payer),
        i128::from(temp_rent) - i128::from(sent.transaction_fee)
    );
}

#[test]
fn native_sell_requires_sync_before_wsol_can_fund_the_next_token_transfer() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let bought = quote(&lab, true, SOL / 10);
    let buy = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, bought.out_amount, lab.authority_token),
        false,
    )]);
    assert!(buy.error.is_none(), "{buy:?}");
    let amount = bought.out_amount / 2;
    let wsol = lab.wsol(lab.user.pubkey());
    let sink = lab.wsol(lab.payer.pubkey());
    let rent = lab.balance(wsol);
    let q = quote(&lab, false, amount);
    let sent = lab.send(&[
        lab.relay(
            lab.swap(false, amount, q.out_amount, lab.authority_token),
            false,
        ),
        transfer(lab.authority, wsol, q.out_amount),
    ]);
    assert!(sent.error.is_none(), "{sent:?}");
    assert_eq!(lab.balance(wsol), rent + q.out_amount);
    assert_eq!(
        lab.tokens(wsol),
        0,
        "lamports alone do not update the token amount"
    );
    let unsynced = lab.send(&[token_transfer(wsol, sink, lab.user.pubkey(), q.out_amount)]);
    assert!(
        unsynced.error.as_deref().unwrap().contains("Custom(1)"),
        "{unsynced:?}"
    );
    let sent = lab.send(&[
        sync(wsol),
        token_transfer(wsol, sink, lab.user.pubkey(), q.out_amount),
        close(wsol, lab.payer.pubkey(), lab.user.pubkey()),
    ]);
    assert!(sent.error.is_none(), "{sent:?}");
    assert_eq!(lab.tokens(sink), q.out_amount);
    assert_eq!(lab.balance(wsol), 0);
    assert_eq!(
        sent.payer_delta,
        i128::from(rent) - i128::from(sent.transaction_fee)
    );
}

#[test]
fn unauthorized_cleanup_rolls_back_a_preceding_successful_curve_swap() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let wsol = lab.wsol(lab.user.pubkey());
    let q = quote(&lab, true, SOL / 10);
    let sent = lab.send(&[
        lab.relay(
            lab.swap(true, SOL / 10, q.out_amount, lab.user_token),
            false,
        ),
        close(wsol, lab.payer.pubkey(), lab.authority), // Wrong close authority.
    ]);
    assert!(
        sent.error.as_deref().unwrap().contains("Custom(4)"),
        "{sent:?}"
    );
    assert_eq!(
        event_trader(&sent),
        lab.authority,
        "logs can contain a swap from a reverted tx"
    );
    assert_eq!(lab.rent(), 0);
    assert_eq!(lab.tokens(lab.user_token), 0);
    assert!(sent.transaction_fee > 0);
}

#[test]
fn downstream_failure_rolls_back_wrapping_cpi_swap_and_trader_account_creation() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let (temp, create) = create_wsol(lab.authority, lab.payer.pubkey());
    let q = quote(&lab, true, SOL / 10);
    let sent = lab.send(&[
        create,
        transfer(lab.authority, temp, SOL / 10),
        sync(temp),
        close(temp, lab.authority, lab.authority),
        lab.relay(
            lab.swap(true, SOL / 10, q.out_amount, lab.user_token),
            false,
        ),
        transfer(lab.user.pubkey(), lab.authority, u64::MAX),
    ]);
    assert!(sent.error.is_some(), "{sent:?}");
    assert!(
        sent.logs
            .iter()
            .any(|l| l.contains("Transfer: insufficient lamports")),
        "failure must happen after swap: {sent:?}"
    );
    assert_eq!(event_trader(&sent), lab.authority);
    assert_eq!(lab.balance(temp), 0);
    assert_eq!(lab.rent(), 0);
    assert_eq!(lab.tokens(lab.user_token), 0);
    assert_eq!(sent.payer_delta, -i128::from(sent.transaction_fee));
}

#[test]
fn forward_clock_change_allows_a_better_fill_but_still_enforces_min_out() {
    let mut lab = Lab::new(Snapshot::load(PROTECTED), false);
    let project = amm_at(&lab.snapshot(), None).0.market().unwrap().project;
    let end = project.launched_at_ts + i64::from(project.protected_launch_duration);
    lab.set_clock(project.launched_at_ts);
    let old = quote(&lab, true, SOL / 10);
    lab.set_clock(end);
    let new = quote(&lab, true, SOL / 10);
    assert!(
        new.out_amount > old.out_amount,
        "forward fee decay improves the fill"
    );
    let failed = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, new.out_amount + 1, lab.user_token),
        false,
    )]);
    assert!(
        failed.error.as_deref().unwrap().contains("Custom(12022)"),
        "{failed:?}"
    );
    let accepted = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, old.out_amount, lab.user_token),
        false,
    )]);
    assert!(accepted.error.is_none(), "{accepted:?}");
    assert_eq!(
        lab.tokens(lab.user_token),
        new.out_amount,
        "better execution exceeds stale quote/min-out"
    );
}

#[test]
fn stale_reserve_quote_fails_min_out_after_an_intervening_buy() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let old = quote(&lab, true, SOL);
    let first = lab.send(&[lab.relay(lab.swap(true, SOL, old.out_amount, lab.user_token), false)]);
    assert!(first.error.is_none(), "{first:?}");
    let new = quote(&lab, true, SOL);
    assert!(new.out_amount < old.out_amount);
    let failed = lab.send(&[lab.relay(lab.swap(true, SOL, old.out_amount, lab.user_token), false)]);
    assert!(
        failed.error.as_deref().unwrap().contains("Custom(12022)"),
        "{failed:?}"
    );
    let retry = lab.send(&[lab.relay(lab.swap(true, SOL, new.out_amount, lab.user_token), false)]);
    assert!(retry.error.is_none(), "{retry:?}");
    assert_eq!(lab.tokens(lab.user_token), old.out_amount + new.out_amount);
}

#[test]
fn prefunded_trader_accounts_are_topped_up_without_losing_donations() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let (rebate, binding) = trader_accounts(&lab.authority);
    // New system accounts must first be rent exempt for zero data. This is still below
    // the rent for the 57/113-byte accounts core allocates, so core must top them up.
    let system_rent = lab.svm.minimum_balance_for_rent_exemption(0);
    let donated = 2 * system_rent;
    let funded = lab.send(&[
        transfer(lab.user.pubkey(), rebate, system_rent),
        transfer(lab.user.pubkey(), binding, system_rent),
    ]);
    assert!(funded.error.is_none(), "{funded:?}");
    let q = quote(&lab, true, SOL / 10);
    let before = lab.balance(lab.authority);
    let sent = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, q.out_amount, lab.user_token),
        false,
    )]);
    assert!(sent.error.is_none(), "{sent:?}");
    assert_eq!(
        before - lab.balance(lab.authority),
        q.in_amount + lab.rent() - donated
    );
    for (key, len) in [(rebate, 57), (binding, 113)] {
        let account = lab.svm.get_account(&key).unwrap();
        assert_eq!(account.owner, loadout_jupiter_amm::LOADOUT_PROGRAM_ID);
        assert_eq!(account.data.len(), len);
        assert_eq!(
            account.lamports,
            lab.svm.minimum_balance_for_rent_exemption(len)
        );
    }
}

#[test]
fn separate_fee_payer_does_not_implicitly_pay_the_core_traders_rent() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let q = quote(&lab, true, SOL / 10);
    let drain = lab.balance(lab.authority) - q.in_amount;
    let drained = lab.send(&[transfer(lab.authority, lab.user.pubkey(), drain)]);
    assert!(drained.error.is_none(), "{drained:?}");
    let sent = lab.send(&[lab.relay(
        lab.swap(true, SOL / 10, q.out_amount, lab.user_token),
        false,
    )]);
    assert!(sent.error.is_some(), "{sent:?}");
    assert!(
        sent.logs
            .iter()
            .any(|l| l.contains("Transfer: insufficient lamports")),
        "{sent:?}"
    );
    assert_eq!(lab.balance(lab.authority), q.in_amount);
    assert_eq!(lab.rent(), 0);
    assert_eq!(sent.payer_delta, -i128::from(sent.transaction_fee));
}

#[test]
fn dust_prefunding_cannot_create_new_rent_paying_system_accounts() {
    let mut lab = Lab::new(Snapshot::load(ACTIVE), false);
    let (rebate, binding) = trader_accounts(&lab.authority);
    let sent = lab.send(&[
        transfer(lab.user.pubkey(), rebate, 1),
        transfer(lab.user.pubkey(), binding, 2),
    ]);
    assert!(
        sent.error
            .as_deref()
            .unwrap()
            .contains("InsufficientFundsForRent"),
        "{sent:?}"
    );
    assert_eq!(lab.balance(rebate), 0);
    assert_eq!(lab.balance(binding), 0);
}

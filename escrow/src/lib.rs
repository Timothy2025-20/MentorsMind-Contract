#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Env, Symbol,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Active,
    Released,
    Disputed,
    Refunded,
    Resolved,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Escrow {
    pub id: u64,
    pub mentor: Address,
    pub learner: Address,
    pub amount: i128,
    pub session_id: Symbol,
    pub status: EscrowStatus,
    pub created_at: u64,
    pub token_address: Address,
    pub platform_fee: i128,
    pub net_amount: i128,
    pub session_end_time: u64,
    pub auto_release_delay: u64,
    pub dispute_reason: Symbol,
    pub resolved_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCreatedEventData {
    pub mentor: Address,
    pub learner: Address,
    pub amount: i128,
    pub session_id: Symbol,
    pub token_address: Address,
    pub session_end_time: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowReleasedEventData {
    pub mentor: Address,
    pub amount: i128,
    pub net_amount: i128,
    pub platform_fee: i128,
    pub token_address: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowAutoReleasedEventData {
    pub time: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeOpenedEventData {
    pub caller: Address,
    pub reason: Symbol,
    pub token_address: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolvedEventData {
    pub mentor_pct: u32,
    pub mentor_amount: i128,
    pub learner_amount: i128,
    pub token_address: Address,
    pub time: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowRefundedEventData {
    pub learner: Address,
    pub amount: i128,
    pub token_address: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSubmittedEventData {
    pub caller: Address,
    pub reason: Symbol,
    pub mentor: Address,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

const ESCROW_COUNT: Symbol = symbol_short!("ESC_CNT");
const ADMIN: Symbol = symbol_short!("ADMIN");
const TREASURY: Symbol = symbol_short!("TREASURY");
const FEE_BPS: Symbol = symbol_short!("FEE_BPS");
const AUTO_REL_DLY: Symbol = symbol_short!("AR_DELAY");
const SESSION_KEY: Symbol = symbol_short!("SESSION");
const APPROVED_TOKEN_KEY: Symbol = symbol_short!("APRV_TOK");

/// Dynamic fee constants
const PRICE_CACHE: Symbol = symbol_short!("PRC_CSH");
const PRICE_CACHE_TIME: Symbol = symbol_short!("PRC_TM");
const DYNAMIC_FEE_ENABLED: Symbol = symbol_short!("DYN_FEE");
const LIQUIDITY_POOL: Symbol = symbol_short!("LIQ_POOL");
const DEFAULT_FEE_BPS: u32 = 500;

/// Maximum configurable fee: 10% = 1 000 basis points.
const MAX_FEE_BPS: u32 = 1_000;

/// Default auto-release delay: 72 hours in seconds.
const DEFAULT_AUTO_RELEASE_DELAY: u64 = 72 * 60 * 60;

// ---------------------------------------------------------------------------
// TTL constants (in ledgers; ~5 s/ledger → 1 000 000 ≈ 57 days)
// ---------------------------------------------------------------------------

const ESCROW_TTL_THRESHOLD: u32 = 500_000;
const ESCROW_TTL_BUMP: u32 = 1_000_000;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    // -----------------------------------------------------------------------
    // Admin / initialization
    // -----------------------------------------------------------------------

    pub fn initialize(
        env: Env,
        admin: Address,
        treasury: Address,
        fee_bps: u32,
        approved_tokens: soroban_sdk::Vec<Address>,
        auto_release_delay_secs: u64,
    ) {
        if env.storage().persistent().has(&ADMIN) {
            panic!("Already initialized");
        }

        if fee_bps > MAX_FEE_BPS {
            panic!("Fee exceeds maximum (1000 bps)");
        }

        env.storage().persistent().set(&ADMIN, &admin);
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.storage().persistent().set(&TREASURY, &treasury);
        env.storage()
            .persistent()
            .extend_ttl(&TREASURY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.storage().persistent().set(&FEE_BPS, &fee_bps);
        env.storage()
            .persistent()
            .extend_ttl(&FEE_BPS, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.storage().persistent().set(&ESCROW_COUNT, &0u64);
        env.storage()
            .persistent()
            .extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let delay = if auto_release_delay_secs == 0 {
            DEFAULT_AUTO_RELEASE_DELAY
        } else {
            auto_release_delay_secs
        };
        env.storage().persistent().set(&AUTO_REL_DLY, &delay);
        env.storage()
            .persistent()
            .extend_ttl(&AUTO_REL_DLY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        for token_addr in approved_tokens.iter() {
            Self::_set_token_approved(&env, &token_addr, true);
        }
    }

    pub fn update_fee(env: Env, new_fee_bps: u32) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        if new_fee_bps > MAX_FEE_BPS {
            panic!("Fee exceeds maximum (1000 bps)");
        }

        env.storage().persistent().set(&FEE_BPS, &new_fee_bps);
        env.storage()
            .persistent()
            .extend_ttl(&FEE_BPS, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
    }

    /// Get dynamic fee based on MNT/USDC price from liquidity pool
    pub fn get_dynamic_fee(env: Env) -> u32 {
        let dynamic_enabled: bool = env
            .storage()
            .instance()
            .get(&DYNAMIC_FEE_ENABLED)
            .unwrap_or(true);

        if !dynamic_enabled {
            return env.storage().persistent().get(&FEE_BPS).unwrap_or(DEFAULT_FEE_BPS);
        }

        let current_ledger = env.ledger().sequence();
        let cached_ledger: u32 = env
            .storage()
            .instance()
            .get(&PRICE_CACHE_TIME)
            .unwrap_or(0);

        if cached_ledger == current_ledger {
            if let Some(cached_price) = env.storage().instance().get::<_, i128>(&PRICE_CACHE) {
                return Self::_calculate_fee_from_price(cached_price);
            }
        }

        let price = Self::_fetch_mnt_usdc_price(&env);

        env.storage().instance().set(&PRICE_CACHE, &price);
        env.storage().instance().set(&PRICE_CACHE_TIME, &current_ledger);

        Self::_calculate_fee_from_price(price)
    }

    fn _calculate_fee_from_price(price: i128) -> u32 {
        if price <= 0 {
            return DEFAULT_FEE_BPS;
        }

        let threshold_010 = 1_000_000;
        let threshold_050 = 5_000_000;
        let threshold_100 = 10_000_000;

        if price < threshold_010 {
            500
        } else if price < threshold_050 {
            400
        } else if price < threshold_100 {
            300
        } else {
            200
        }
    }

    fn _fetch_mnt_usdc_price(env: &Env) -> i128 {
        let pool_address: Option<Address> = env.storage().instance().get(&LIQUIDITY_POOL);
        if let Some(_pool) = pool_address {
            // TODO: Implement actual pool contract integration
            // Placeholder: return $0.75 (7,500,000) for testing
            7_500_000
        } else {
            0
        }
    }

    /// Set liquidity pool address (admin only)
    pub fn set_liquidity_pool(env: Env, pool_address: Address) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        admin.require_auth();
        env.storage().instance().set(&LIQUIDITY_POOL, &pool_address);
    }

    /// Enable or disable dynamic fee (admin only)
    pub fn set_dynamic_fee_enabled(env: Env, enabled: bool) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        admin.require_auth();
        env.storage().instance().set(&DYNAMIC_FEE_ENABLED, &enabled);
    }

    pub fn update_treasury(env: Env, new_treasury: Address) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        env.storage().persistent().set(&TREASURY, &new_treasury);
        env.storage()
            .persistent()
            .extend_ttl(&TREASURY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
    }

    pub fn set_approved_token(env: Env, token_address: Address, approved: bool) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        Self::_set_token_approved(&env, &token_address, approved);
    }

    // -----------------------------------------------------------------------
    // Escrow lifecycle
    // -----------------------------------------------------------------------

    pub fn create_escrow(
        env: Env,
        mentor: Address,
        learner: Address,
        amount: i128,
        session_id: Symbol,
        token_address: Address,
        session_end_time: u64,
    ) -> u64 {
        if amount <= 0 {
            panic!("Amount must be greater than zero");
        }

        if !Self::_is_token_approved(&env, &token_address) {
            panic!("Token not approved");
        }

        learner.require_auth();

        let token_client = token::Client::new(&env, &token_address);
        let learner_balance = token_client.balance(&learner);
        if learner_balance < amount {
            panic!("Insufficient token balance");
        }

        let auto_release_delay: u64 = env
            .storage()
            .persistent()
            .get(&AUTO_REL_DLY)
            .unwrap_or(DEFAULT_AUTO_RELEASE_DELAY);
        env.storage()
            .persistent()
            .extend_ttl(&AUTO_REL_DLY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let session_key = (SESSION_KEY, session_id.clone());
        if env.storage().persistent().has(&session_key) {
            panic!("Session ID already exists");
        }
        env.storage().persistent().set(&session_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&session_key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut count: u64 = env.storage().persistent().get(&ESCROW_COUNT).unwrap_or(0);
        count = count.checked_add(1).expect("Counter overflow");
        env.storage().persistent().set(&ESCROW_COUNT, &count);
        env.storage()
            .persistent()
            .extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        token_client.transfer(&learner, &env.current_contract_address(), &amount);

        let escrow = Escrow {
            id: count,
            mentor: mentor.clone(),
            learner: learner.clone(),
            amount,
            session_id: session_id.clone(),
            status: EscrowStatus::Active,
            created_at: env.ledger().timestamp(),
            token_address: token_address.clone(),
            platform_fee: 0,
            net_amount: 0,
            session_end_time,
            auto_release_delay,
            dispute_reason: symbol_short!(""),
            resolved_at: 0,
        };

        let key = (symbol_short!("ESCROW"), count);
        env.storage().persistent().set(&key, &escrow);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.events().publish(
            (Symbol::new(&env, "Escrow"), Symbol::new(&env, "Created"), count),
            EscrowCreatedEventData {
                mentor,
                learner,
                amount,
                session_id,
                token_address,
                session_end_time,
            },
        );

        count
    }

    pub fn release_funds(env: Env, caller: Address, escrow_id: u64) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Admin not found");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        caller.require_auth();
        if caller != escrow.learner && caller != admin {
            panic!("Caller not authorized");
        }

        Self::_do_release(&env, &mut escrow, &key);
    }

    pub fn release_partial(env: Env, caller: Address, escrow_id: u64, amount_to_release: i128) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        if amount_to_release <= 0 || amount_to_release > escrow.amount {
            panic!("Invalid release amount");
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Admin not found");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        caller.require_auth();
        if caller != escrow.learner && caller != admin {
            panic!("Caller not authorized");
        }

        let fee_bps: u32 = Self::get_dynamic_fee(env.clone());

        let platform_fee: i128 = amount_to_release
            .checked_mul(fee_bps as i128)
            .expect("Overflow")
            .checked_div(10_000)
            .expect("Division error");
        let net_amount: i128 = amount_to_release
            .checked_sub(platform_fee)
            .expect("Underflow");

        let treasury: Address = env
            .storage()
            .persistent()
            .get(&TREASURY)
            .expect("Treasury not found");
        env.storage()
            .persistent()
            .extend_ttl(&TREASURY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let token_client = token::Client::new(&env, &escrow.token_address);

        if platform_fee > 0 {
            token_client.transfer(&env.current_contract_address(), &treasury, &platform_fee);
        }

        token_client.transfer(&env.current_contract_address(), &escrow.mentor, &net_amount);

        escrow.amount = escrow.amount.checked_sub(amount_to_release).expect("Underflow");
        escrow.platform_fee = escrow.platform_fee.checked_add(platform_fee).expect("Overflow");
        escrow.net_amount = escrow.net_amount.checked_add(net_amount).expect("Overflow");

        if escrow.amount == 0 {
            escrow.status = EscrowStatus::Released;
        }

        env.storage().persistent().set(&key, &escrow);

        env.events().publish(
            (symbol_short!("Escrow"), symbol_short!("rel_part"), escrow.id),
            (
                escrow.mentor.clone(),
                amount_to_release,
                net_amount,
                platform_fee,
                escrow.token_address.clone(),
                escrow.amount,
            ),
        );
    }

    pub fn admin_release(env: Env, escrow_id: u64) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Admin not found");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        env.events().publish(
            (symbol_short!("Escrow"), symbol_short!("adm_rel"), escrow_id),
            (escrow_id, env.ledger().timestamp()),
        );

        Self::_do_release(&env, &mut escrow, &key);
    }

    pub fn try_auto_release(env: Env, escrow_id: u64) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        let now = env.ledger().timestamp();
        let release_after = escrow
            .session_end_time
            .checked_add(escrow.auto_release_delay)
            .expect("Timestamp overflow");

        if now < release_after {
            panic!("Auto-release window has not elapsed");
        }

        env.events().publish(
            (Symbol::new(&env, "Escrow"), Symbol::new(&env, "AutoReleased"), escrow_id),
            EscrowAutoReleasedEventData { time: now },
        );

        Self::_do_release(&env, &mut escrow, &key);
    }

    pub fn dispute(env: Env, caller: Address, escrow_id: u64, reason: Symbol) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        caller.require_auth();
        if caller != escrow.mentor && caller != escrow.learner {
            panic!("Caller not authorized to dispute");
        }

        escrow.status = EscrowStatus::Disputed;
        escrow.dispute_reason = reason.clone();
        env.storage().persistent().set(&key, &escrow);

        env.events().publish(
            (Symbol::new(&env, "Escrow"), Symbol::new(&env, "DisputeOpened"), escrow_id),
            DisputeOpenedEventData {
                caller,
                reason,
                token_address: escrow.token_address,
            },
        );
    }

    pub fn resolve_dispute(env: Env, escrow_id: u64, release_to_mentor: bool) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Not initialized");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Disputed {
            panic!("Escrow is not in Disputed status");
        }

        let now = env.ledger().timestamp();

        if release_to_mentor {
            Self::_do_release(&env, &mut escrow, &key);
            escrow.status = EscrowStatus::Resolved;
            escrow.resolved_at = now;
            env.storage().persistent().set(&key, &escrow);

            env.events().publish(
                (symbol_short!("Escrow"), symbol_short!("disp_res"), escrow_id),
                (
                    escrow_id,
                    release_to_mentor,
                    escrow.net_amount,
                    0i128,
                    escrow.token_address.clone(),
                    now,
                ),
            );
        } else {
            let token_client = token::Client::new(&env, &escrow.token_address);
            token_client.transfer(
                &env.current_contract_address(),
                &escrow.learner,
                &escrow.amount,
            );
            escrow.status = EscrowStatus::Resolved;
            escrow.net_amount = 0;
            escrow.platform_fee = escrow.amount;
            escrow.resolved_at = now;
            env.storage().persistent().set(&key, &escrow);

            env.events().publish(
                (symbol_short!("Escrow"), symbol_short!("disp_res"), escrow_id),
                (
                    escrow_id,
                    release_to_mentor,
                    0i128,
                    escrow.amount,
                    escrow.token_address.clone(),
                    now,
                ),
            );
        }
    }

    pub fn refund(env: Env, escrow_id: u64) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&ADMIN)
            .expect("Admin not found");
        env.storage()
            .persistent()
            .extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let mut escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status == EscrowStatus::Released
            || escrow.status == EscrowStatus::Refunded
            || escrow.status == EscrowStatus::Resolved
        {
            panic!("Cannot refund");
        }

        let token_client = token::Client::new(&env, &escrow.token_address);
        token_client.transfer(
            &env.current_contract_address(),
            &escrow.learner,
            &escrow.amount,
        );

        escrow.status = EscrowStatus::Refunded;
        env.storage().persistent().set(&key, &escrow);

        env.events().publish(
            (Symbol::new(&env, "Escrow"), Symbol::new(&env, "Refunded"), escrow_id),
            EscrowRefundedEventData {
                learner: escrow.learner.clone(),
                amount: escrow.amount,
                token_address: escrow.token_address,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    pub fn get_escrow(env: Env, escrow_id: u64) -> Escrow {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found")
    }

    pub fn get_escrow_count(env: Env) -> u64 {
        env.storage()
            .persistent()
            .extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage().persistent().get(&ESCROW_COUNT).unwrap_or(0)
    }

    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .extend_ttl(&FEE_BPS, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage().persistent().get(&FEE_BPS).unwrap_or(0)
    }

    pub fn get_treasury(env: Env) -> Address {
        env.storage()
            .persistent()
            .extend_ttl(&TREASURY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage()
            .persistent()
            .get(&TREASURY)
            .expect("Treasury not set")
    }

    pub fn get_auto_release_delay(env: Env) -> u64 {
        env.storage()
            .persistent()
            .extend_ttl(&AUTO_REL_DLY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage()
            .persistent()
            .get(&AUTO_REL_DLY)
            .unwrap_or(DEFAULT_AUTO_RELEASE_DELAY)
    }

    pub fn is_token_approved(env: Env, token_address: Address) -> bool {
        Self::_is_token_approved(&env, &token_address)
    }

    pub fn get_escrows_by_mentor(env: Env, mentor: Address) -> Vec<Escrow> {
        let count = env.storage().persistent().get(&ESCROW_COUNT).unwrap_or(0u64);
        let mut result = Vec::new(&env);

        for i in 1..=count {
            let key = (symbol_short!("ESCROW"), i);
            if let Some(escrow) = env.storage().persistent().get::<_, Escrow>(&key) {
                if escrow.mentor == mentor {
                    result.push_back(escrow);
                }
            }
        }

        result
    }

    pub fn submit_review(env: Env, caller: Address, escrow_id: u64, reason: Symbol) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let escrow: Escrow = env
            .storage()
            .persistent()
            .get(&key)
            .expect("Escrow not found");

        caller.require_auth();
        if caller != escrow.learner {
            panic!("Only learner can submit review");
        }

        if escrow.status != EscrowStatus::Released {
            panic!("Can only review released escrows");
        }

        let review_key = (symbol_short!("REVIEW"), escrow_id);
        env.storage().persistent().set(&review_key, &reason);
        env.storage()
            .persistent()
            .extend_ttl(&review_key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.events().publish(
            (Symbol::new(&env, "Escrow"), Symbol::new(&env, "ReviewSubmitted"), escrow_id),
            ReviewSubmittedEventData {
                caller,
                reason,
                mentor: escrow.mentor,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn _do_release(env: &Env, escrow: &mut Escrow, key: &(Symbol, u64)) {
        let release_amount = escrow.amount;
        let fee_bps: u32 = Self::get_dynamic_fee(env.clone());

        let platform_fee: i128 = release_amount
            .checked_mul(fee_bps as i128)
            .expect("Overflow")
            .checked_div(10_000)
            .expect("Division error");
        let net_amount: i128 = release_amount
            .checked_sub(platform_fee)
            .expect("Underflow");

        let treasury: Address = env
            .storage()
            .persistent()
            .get(&TREASURY)
            .expect("Treasury not found");
        env.storage()
            .persistent()
            .extend_ttl(&TREASURY, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        let token_client = token::Client::new(env, &escrow.token_address);

        if platform_fee > 0 {
            token_client.transfer(&env.current_contract_address(), &treasury, &platform_fee);
        }

        token_client.transfer(&env.current_contract_address(), &escrow.mentor, &net_amount);

        escrow.status = EscrowStatus::Released;
        escrow.platform_fee = escrow.platform_fee.checked_add(platform_fee).expect("Overflow");
        escrow.net_amount = escrow.net_amount.checked_add(net_amount).expect("Overflow");
        escrow.amount = 0;
        env.storage().persistent().set(key, escrow);

        env.events().publish(
            (Symbol::new(env, "Escrow"), Symbol::new(env, "Released"), escrow.id),
            EscrowReleasedEventData {
                mentor: escrow.mentor.clone(),
                amount: release_amount,
                net_amount,
                platform_fee,
                token_address: escrow.token_address.clone(),
            },
        );
    }

    fn _set_token_approved(env: &Env, token_address: &Address, approved: bool) {
        let key = (APPROVED_TOKEN_KEY, token_address.clone());
        env.storage().persistent().set(&key, &approved);
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
    }

    fn _is_token_approved(env: &Env, token_address: &Address) -> bool {
        let key = (APPROVED_TOKEN_KEY, token_address.clone());
        env.storage()
            .persistent()
            .get::<_, bool>(&key)
            .unwrap_or(false)
    }
}
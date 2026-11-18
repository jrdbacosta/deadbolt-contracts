# DeadBolt — Project Blueprint & Roadmap

## 1. Vision & Core Philosophy ("Simple Stupid")

- **Mission:** DeadBolt is a 100% on-chain, non-custodial recovery, inheritance, and panic vault on Solana.
- **Asset promise:** Protect SOL, USDC, USDT, and all Solana SPL tokens in a single non-custodial vault. Any classic SPL Token Program mint is supported (each held in an ATA owned by the vault PDA). Token-2022 is not supported yet.
- **Design Principle:** Keep it radically simple. No off-chain databases, no user accounts, no KYC, and no fiat price estimates. Everything lives on-chain.

## 2. Smart Contract Rules (`lib.rs`)

- **Program ID:** `ARNGGV2hi9ndYSm95cGqTcq1audRoNsgt1sstoCZuKpq`
- **Config PDA (devnet):** `CkZx1adhhE5FnmX71MH6jmJGxrbd5icM3Doy4bryvHne` (seeds `[b"config", authority]`)
- **100% Non-Custodial:** Funds live in isolated Vault PDAs (`[b"vault", owner, vault_id]`). There is NO admin withdrawal function.

### 2.1 Pricing (SOL-only)
- **Annual Fee:** `0.15 SOL / year` (`150,000,000 lamports`), adjustable via `update_config`. Year 1 is charged in `create_vault`; renewals are auto-debited from the vault by `check_in`, `deposit` or `owner_withdraw` when overdue (`RenewalProcessed`).
- **Emergency Rescue / Claim Fee:** default `0.5%` (`50 bps`), taken only when `trigger_kill_switch`, `claim_expired_vault`, or `sweep_tokens` executes.
  - Adjustable by the admin via `update_config(..., rescue_fee_bps: Option<u16>)`.
  - **Hard trust cap:** `require!(bps <= MAX_RESCUE_FEE_BPS /* 500 */, InvalidBps)` — the rescue fee can never exceed 5%. This is enforced by the program, not the UI.
  - 100% of the rescue fee goes to `platform_treasury`. **Partners never earn from rescue fees.**
- **Zero-fee actions (network gas only):** `check_in`, `update_timeout`, `deposit`, `owner_withdraw` (0% withdraw fee).
- **Safety Guarantee:** Unpaid annual renewals NEVER block `trigger_kill_switch`, `claim_expired_vault`, or `owner_withdraw`.

### 2.2 Partner commission (annual fee only)
- **Default rate:** `DEFAULT_PARTNER_BPS = 2000` (20%) for new configs; stored globally as `config.partner_bps` and adjustable via `update_config`. The existing devnet config keeps whatever value it was initialized with until updated.
- **Per-partner custom tiers — `PartnerTier` PDA** (seeds `[b"partner", partner_wallet]`): `partner_wallet: Pubkey`, `custom_bps: u16`, `is_active: bool`, `bump: u8`.
  - `set_partner_tier(partner_wallet, custom_bps)` — admin only (`has_one = admin` on config), `custom_bps <= 10_000`, `init_if_needed` (admin pays rent). Emits `PartnerTierSet`.
  - `clear_partner_tier(partner_wallet)` — admin only; closes the PDA (rent back to admin) so the partner falls back to `config.partner_bps`. Emits `PartnerTierSet { is_active: false }`.
- **Effective rate:** `create_vault`, `deposit`, `owner_withdraw` and `check_in` take a `partner_tier` account as their **last** account, constrained to `seeds = [b"partner", partner.key()]`. If that PDA exists, is owned by the program and `is_active`, `custom_bps` is used; otherwise `config.partner_bps`.
  - The account is **always passed** (it may be an empty PDA). This is deliberate: an `Option` account would let the transaction builder omit it and silently pay the partner the default rate. The seed constraint prevents substituting another partner's tier.
  - When a vault has no partner, `partner = platform_treasury` and the partner share is 0 regardless of any tier.
- Frontend: tier helpers live in `src/lib/partnerTiers.ts`; `DeadBoltSDK.derivePartnerTier` is used for the four fee-bearing instructions.

### 2.3 Free timer changes (`update_timeout`)
- `update_timeout(new_timeout_seconds: i64)` — owner only (`has_one = owner`, `has_one = config`), vault must be active, `MIN_TIMEOUT (60s) ..= MAX_TIMEOUT (3650 days)`.
- Sets `vault.timeout_seconds` **and** resets `vault.last_check_in = now`, so shortening the timer can never cause an instant expiry.
- No platform fee, no lamport movement, no renewal processing. Emits `TimeoutUpdated`.

### 2.4 Who can do what (enforced on-chain)
- `check_in`, `update_timeout`, `owner_withdraw`, `owner_withdraw_token` — owner only.
- `trigger_kill_switch` — owner **or** `safe_wallet`; funds go to `safe_wallet`.
- `claim_expired_vault` — any signer once `now > last_check_in + timeout_seconds`; funds always go to `backup_wallet`.
- `update_config`, `set_partner_tier`, `clear_partner_tier` — only the wallet stored in `config.admin`.

### 2.5 Upgrades
- **Compatibility:** Never reorder/resize `Config` or `Vault` fields, reorder `ErrorCode` variants, or rename existing instructions/events. New instructions, accounts, args (appended), events and errors are appended only.
- **Upgrading a larger build in place:** if the new `.so` is bigger than the deployed ProgramData, the upgrade fails with `invalid program argument`. Expand storage first, with **Admin → Ledger & Config → Program upgrade → Expand program storage** (signed by the upgrade authority) or `solana program extend <PROGRAM_ID> <BYTES>`, then redeploy.
- **Storage sizing:** expand only when an upgrade needs it (each byte locks ~0.00000696 SOL of rent). For mainnet, reserve ~25–50% headroom at first deploy via `--max-len`.
- After every upgrade, copy `contracts/target/idl/workspace.json` → `src/idl/deadboltIDL.json` and `contracts/target/types/workspace.ts` → `src/idl/deadboltTypes.ts`.

## 3. Application Structure

- `/` — Landing page (Hero with SPL messaging, feature cards, Trust-first Q&A), **Create Vault**, and **My Vaults & Panic Dashboard** (SOL + SPL token management).
- `/partners` — Referral link generator (`?ref=<WALLET>`) and on-chain earnings tracker. Shows the partner's real rate (custom tier if active, else default) and states that commission comes from annual fees only.
- `/admin` — Wallet-gated (on-chain `config.admin`) dashboard with 4 tabs (see 4.6). The footer Admin link is only shown to that wallet.

## 4. Frontend Rules That Must Be Preserved

These rules are load-bearing. Future updates must not regress them.

### 4.1 Multi-role vault discovery
- `DeadBoltSDK.getVaultsForWallet` reads **every** Vault account and filters client-side.
- A vault is shown when the connected wallet matches **any** of: `owner` (**Role: Owner**), `backup_wallet` (**Role: Backup Beneficiary** — Claim Expired Vault once expired), `safe_wallet` (**Role: Safe Panic Wallet** — Trigger Kill Switch while active). Multiple roles show every matching badge and action.

### 4.2 Change Timer (owner, free)
- Presets: 30 Days, 90 Days, 180 Days, 1 Year, custom whole days (1–3650), and **1 Min (Test)** on test clusters only (see 4.7).
- Always states **"No DeadBolt fee — network gas only"** and that the countdown restarts from now.

### 4.3 Optional safe wallet
- `backup_wallet` is the single required destination. The separate kill-switch wallet toggle is **off by default**; when off, `backup_wallet` is passed as `safe_wallet`.

### 4.4 SOL-only pricing transparency
- The Create Vault summary is an itemized receipt: Vault Deposit (100% yours), Year 1 Protocol Fee (`config.annual_fee_lamports`), Emergency Rescue / Claim Fee (`config.rescue_fee_bps`, shown with the 5% cap).
- No fiat conversions or USD estimates anywhere.

### 4.5 Transaction feedback
- Every transaction shows a toast with a **View on Solscan** link (cluster-aware).
- Program errors are translated in `src/lib/errors.ts`. Anchor codes start at `6000` in `ErrorCode` order — keep in sync.

### 4.6 `/admin` — 4 tabs, 100% Solana RPC (no database, no indexer)
Data is loaded by `useProtocolMonitor` (`src/hooks/useProtocolMonitor.ts`) with helpers in `src/lib/adminAnalytics.ts`. Ledger history loads the latest ~600 program signatures per refresh; older pages load on demand, and fee totals cover only loaded history.

1. **Overview & Business Analytics** (`OverviewTab`)
   - TVL: SOL locked across non-swept vaults (above-rent balance) + per-mint SPL breakdown (BigInt sums).
   - Revenue & growth: Year-1 fees collected (`VaultCreated.fee_paid`, renewals shown separately), Projected annual renewal revenue (active vaults × annual fee), Rescue fees collected, Direct vs partner-referred ratio.
   - Health radar: Active, Expiring in < 7 days, Expired/unclaimed, Swept.
2. **Vaults Inspector** (`VaultInspectorTable`) — search by vault/ID/owner/backup/safe/partner, filter by status; Vault ID, Owner, Backup/Safe, SOL + SPL balances, live countdown + status badge, Solscan link.
3. **Partners & Tier Rewards** (`PartnersTab`)
   - Leaderboard grouped by `vault.partner_wallet` (plus partners with a tier and no referrals yet): referred vaults, SOL locked by referrals, commission earned, current tier (Default X% / Custom Y%).
   - **Commission earned is exact:** the partner wallet's lamport delta in each `VaultCreated` / `RenewalProcessed` transaction (skipped if the partner signed it). Rescue fees never count.
   - Inline **Adjust Tier** (`PartnerTierControl`): 20% / 25% / 30% pills, custom %, Reset (→ `clear_partner_tier`). A form sets a tier for a partner before their first referral.
4. **Ledger & Config** (`LedgerConfigTab`)
   - Filterable event ledger: `VaultCreated`, `CheckedIn`, `TimeoutUpdated`, `Deposited`, `Withdrawn`, `RenewalProcessed`, `KillSwitchTriggered`, `VaultClaimed`, `TokensSwept`, `PartnerTierSet`, plus `UpdateConfig` / `Failed`. SOL amounts to 9 decimals (lamport-exact), Solscan links.
   - Global settings form (`update_config`): Annual Fee (SOL), Default Partner %, Emergency Rescue Fee % (0–5%, validated client-side and on-chain), Treasury and Admin transfer. Only changed fields are sent.
   - Live config overview + Program upgrade card (storage size, last deployed slot, upgrade authority, Expand storage).

### 4.7 Test-only UI
- `IS_TEST_CLUSTER` (`src/lib/constants.ts`) is true only when `CLUSTER` is devnet/localnet **and** the RPC URL points to devnet or localhost (never if it contains `mainnet`).
- The **1 Min (Test)** preset (Create Vault and Change Timer) renders only when `IS_TEST_CLUSTER`, and Create Vault ignores the test timeout otherwise.

### 4.8 Code layout notes
- `src/lib/deadbolt.ts` (main SDK) is very large; add new feature code in focused modules (`partnerTiers.ts`, `adminAnalytics.ts`, `programStorage.ts`) instead of growing it.

## 5. Roadmap

- **Phase 1 (Current — Devnet Validation):** End-to-end testing of vault creation, 1-minute test claims, kill switch sweeps, SPL sweeps, partner splits incl. custom tiers, rescue fee changes, and admin config updates.
- **Phase 2 (Mainnet Launch):** Deploy the verified program to Mainnet-Beta with `--max-len` headroom, initialize production `Config` (default partner 20%, rescue 0.5%), switch `CLUSTER`/RPC (test UI hides automatically), publish the IDL for trustless verification.
- **Phase 2.5 — V2: Tiered Multi-Vault Sharding (next post-mainnet milestone):** see section 6.
- **Phase 3 (Future):**
  - Optional off-chain check-in reminders (Telegram / email from public wallet events).
  - Token-2022 (SPL Token Extensions) support.
  - Multi-sig / Squads integration for safe cold wallets.
  - Indexed history for all-time revenue and commission totals beyond the RPC window.

## 6. V2 Roadmap — Fortress Multi-Shard Protection

**Status:** Not started. This is the first milestone after mainnet launch. The Create Vault page shows a locked teaser card (`src/components/FortressTeaser.tsx`, "COMING SOON • V2") with greyed-out tiers. It doesn't change how vaults are created today.

**Goal:** Split one deposit across several isolated Vault PDAs, so a single compromised signer or exploit path can't drain a high-value treasury in one transaction.

**Tiers**
- **Standard:** 1 vault, the current V1 behaviour. Remains the default.
- **Fortress:** 5 shards.
- **Whale:** 20+ shards, for treasuries and DAOs.

**Planned protections**
- **Tripwire lockdown:** suspicious activity on any one shard locks down the whole group, and owner withdrawals stop until the owner or safe wallet resolves it. Kill switch and expiry claims must still work during a lockdown, per the V1 safety guarantee.
- **Withdrawal rate-limiting (anti-drain):** a cap on how much an owner can withdraw per time window across the shard group, so a stolen owner key can only move a limited amount before the tripwire or kill switch takes over.

**Design constraints carried over from V1**
- Fully non-custodial with no admin withdrawal. Shard groups are on-chain only, with no off-chain coordinator.
- The upgrade stays layout-compatible: new accounts and instructions are added after the existing ones, and existing `Vault` / `Config` layouts don't change. Existing single vaults keep working as Standard.
- The public UI shows only the tiers and what they're for. Detailed mechanics (thresholds, window sizes, how shards are spread) stay out of the marketing copy until V2 ships.
- Pricing for Fortress and Whale is still to be decided. It will be SOL-only, and the 5% hard cap on the rescue fee still applies.

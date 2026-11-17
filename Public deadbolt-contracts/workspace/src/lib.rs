use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};

declare_id!("ARNGGV2hi9ndYSm95cGqTcq1audRoNsgt1sstoCZuKpq");

pub const SECONDS_PER_DAY: i64 = 86_400;
pub const SECONDS_PER_YEAR: i64 = 365 * SECONDS_PER_DAY;
pub const MIN_TIMEOUT: i64 = 60;
pub const MAX_TIMEOUT: i64 = 3650 * SECONDS_PER_DAY;
pub const RESCUE_FEE_BPS: u16 = 50;
pub const DEFAULT_PARTNER_BPS: u16 = 2000;
pub const MAX_RESCUE_FEE_BPS: u16 = 500;
pub const BPS_DENOMINATOR: u128 = 10_000;

#[program]
pub mod workspace {
    use super::*;

    // annual_fee_lamports: u64, Annual vault subscription fee in lamports, 150000000 = 0.15 SOL
    // partner_bps: u16, Partner share of annual fee in basis points (default 2000 = 20%), 2000 = 20%
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        annual_fee_lamports: u64,
        partner_bps: u16,
    ) -> Result<()> {
        require!(partner_bps <= 10_000, ErrorCode::InvalidBps);
        let authority_key = ctx.accounts.authority.key();
        let config = &mut ctx.accounts.config;
        config.admin = authority_key;
        config.platform_treasury = authority_key;
        config.annual_fee_lamports = annual_fee_lamports;
        config.partner_bps = partner_bps;
        config.rescue_fee_bps = RESCUE_FEE_BPS;
        config.bump = ctx.bumps.config;
        config.authority = authority_key;
        config.is_active = true;
        config.is_paused = false;
        config.version = 1;
        Ok(())
    }

    pub fn update_config(
        ctx: Context<UpdateConfig>,
        new_treasury: Option<Pubkey>,
        annual_fee_lamports: Option<u64>,
        partner_bps: Option<u16>,
        new_admin: Option<Pubkey>,
        rescue_fee_bps: Option<u16>,
    ) -> Result<()> {
        if let Some(bps) = partner_bps {
            require!(bps <= 10_000, ErrorCode::InvalidBps);
        }
        if let Some(bps) = rescue_fee_bps {
            require!(bps <= MAX_RESCUE_FEE_BPS, ErrorCode::InvalidBps);
        }
        if let Some(t) = new_treasury {
            require!(t != Pubkey::default(), ErrorCode::InvalidWallet);
        }
        if let Some(a) = new_admin {
            require!(a != Pubkey::default(), ErrorCode::InvalidWallet);
        }
        let config = &mut ctx.accounts.config;
        if let Some(t) = new_treasury {
            config.platform_treasury = t;
        }
        if let Some(fee) = annual_fee_lamports {
            config.annual_fee_lamports = fee;
        }
        if let Some(bps) = partner_bps {
            config.partner_bps = bps;
        }
        if let Some(a) = new_admin {
            config.admin = a;
        }
        if let Some(bps) = rescue_fee_bps {
            config.rescue_fee_bps = bps;
        }
        Ok(())
    }

    pub fn set_partner_tier(
        ctx: Context<SetPartnerTier>,
        partner_wallet: Pubkey,
        custom_bps: u16,
    ) -> Result<()> {
        require!(custom_bps <= 10_000, ErrorCode::InvalidBps);
        require!(partner_wallet != Pubkey::default(), ErrorCode::InvalidPartner);
        let admin_key = ctx.accounts.admin.key();
        let tier = &mut ctx.accounts.partner_tier;
        tier.partner_wallet = partner_wallet;
        tier.custom_bps = custom_bps;
        tier.is_active = true;
        tier.bump = ctx.bumps.partner_tier;
        emit!(PartnerTierSet {
            partner_wallet,
            custom_bps,
            is_active: true,
            admin: admin_key,
        });
        Ok(())
    }

    pub fn clear_partner_tier(ctx: Context<ClearPartnerTier>, partner_wallet: Pubkey) -> Result<()> {
        emit!(PartnerTierSet {
            partner_wallet,
            custom_bps: 0,
            is_active: false,
            admin: ctx.accounts.admin.key(),
        });
        Ok(())
    }

    pub fn create_vault(
        ctx: Context<CreateVault>,
        vault_id: u64,
        backup_wallet: Pubkey,
        safe_wallet: Pubkey,
        partner_wallet: Option<Pubkey>,
        timeout_seconds: i64,
        initial_deposit_lamports: u64,
    ) -> Result<()> {
        let owner_key = ctx.accounts.owner.key();
        let config = &ctx.accounts.config;
        let now = Clock::get()?.unix_timestamp;

        require!(
            (MIN_TIMEOUT..=MAX_TIMEOUT).contains(&timeout_seconds),
            ErrorCode::InvalidTimeout
        );
        require!(
            backup_wallet != owner_key && safe_wallet != owner_key,
            ErrorCode::InvalidWallet
        );
        require!(
            backup_wallet != Pubkey::default() && safe_wallet != Pubkey::default(),
            ErrorCode::InvalidWallet
        );
        if let Some(p) = partner_wallet {
            require!(p != owner_key && p != Pubkey::default(), ErrorCode::InvalidPartner);
        }
        require!(
            ctx.accounts.partner.key() == partner_wallet.unwrap_or(config.platform_treasury),
            ErrorCode::InvalidFeeRecipient
        );

        let fee = config.annual_fee_lamports;
        let bps = effective_partner_bps(config, &ctx.accounts.partner_tier.to_account_info())?;
        let (partner_share, treasury_share) = split_fee(fee, bps, partner_wallet.is_some())?;

        if partner_share > 0 {
            system_transfer(
                &ctx.accounts.system_program,
                ctx.accounts.owner.to_account_info(),
                ctx.accounts.partner.to_account_info(),
                partner_share,
            )?;
        }
        if treasury_share > 0 {
            system_transfer(
                &ctx.accounts.system_program,
                ctx.accounts.owner.to_account_info(),
                ctx.accounts.platform_treasury.to_account_info(),
                treasury_share,
            )?;
        }
        if initial_deposit_lamports > 0 {
            system_transfer(
                &ctx.accounts.system_program,
                ctx.accounts.owner.to_account_info(),
                ctx.accounts.vault.to_account_info(),
                initial_deposit_lamports,
            )?;
        }

        let config_key = ctx.accounts.config.key();
        let vault = &mut ctx.accounts.vault;
        vault.owner = owner_key;
        vault.backup_wallet = backup_wallet;
        vault.safe_wallet = safe_wallet;
        vault.vault_id = vault_id;
        vault.partner_wallet = partner_wallet;
        vault.timeout_seconds = timeout_seconds;
        vault.last_check_in = now;
        vault.paid_until = now.checked_add(SECONDS_PER_YEAR).ok_or(ErrorCode::MathOverflow)?;
        vault.created_at = now;
        vault.is_active = true;
        vault.swept_to = None;
        vault.bump = ctx.bumps.vault;
        vault.config = config_key;

        emit!(VaultCreated {
            vault: vault.key(),
            owner: owner_key,
            vault_id,
            backup_wallet,
            safe_wallet,
            partner_wallet,
            timeout_seconds,
            fee_paid: fee,
            initial_deposit: initial_deposit_lamports,
            paid_until: vault.paid_until,
        });
        Ok(())
    }

    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        let depositor_key = ctx.accounts.depositor.key();
        let is_owner = depositor_key == ctx.accounts.vault.owner;
        require!(amount > 0, ErrorCode::InvalidAmount);

        system_transfer(
            &ctx.accounts.system_program,
            ctx.accounts.depositor.to_account_info(),
            ctx.accounts.vault.to_account_info(),
            amount,
        )?;

        let now = Clock::get()?.unix_timestamp;
        if is_owner {
            let bps = effective_partner_bps(
                &ctx.accounts.config,
                &ctx.accounts.partner_tier.to_account_info(),
            )?;
            ctx.accounts.vault.last_check_in = now;
            process_renewal(
                &ctx.accounts.config,
                &mut ctx.accounts.vault,
                &ctx.accounts.platform_treasury.to_account_info(),
                &ctx.accounts.partner.to_account_info(),
                now,
                bps,
            )?;
        }

        emit!(Deposited {
            vault: ctx.accounts.vault.key(),
            depositor: depositor_key,
            mint: None,
            amount,
        });
        Ok(())
    }

    pub fn owner_withdraw(ctx: Context<OwnerWithdraw>, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::InvalidAmount);
        let now = Clock::get()?.unix_timestamp;
        let bps = effective_partner_bps(
            &ctx.accounts.config,
            &ctx.accounts.partner_tier.to_account_info(),
        )?;

        process_renewal(
            &ctx.accounts.config,
            &mut ctx.accounts.vault,
            &ctx.accounts.platform_treasury.to_account_info(),
            &ctx.accounts.partner.to_account_info(),
            now,
            bps,
        )?;

        let vault_info = ctx.accounts.vault.to_account_info();
        let spendable = spendable_lamports(&vault_info)?;
        require!(amount <= spendable, ErrorCode::InsufficientVaultBalance);

        move_lamports(&vault_info, &ctx.accounts.owner.to_account_info(), amount)?;

        let vault = &mut ctx.accounts.vault;
        vault.last_check_in = now;

        emit!(Withdrawn {
            vault: vault.key(),
            owner: vault.owner,
            mint: None,
            amount,
        });
        Ok(())
    }

    pub fn check_in(ctx: Context<CheckIn>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let bps = effective_partner_bps(
            &ctx.accounts.config,
            &ctx.accounts.partner_tier.to_account_info(),
        )?;
        ctx.accounts.vault.last_check_in = now;

        process_renewal(
            &ctx.accounts.config,
            &mut ctx.accounts.vault,
            &ctx.accounts.platform_treasury.to_account_info(),
            &ctx.accounts.partner.to_account_info(),
            now,
            bps,
        )?;

        emit!(CheckedIn {
            vault: ctx.accounts.vault.key(),
            owner: ctx.accounts.vault.owner,
            timestamp: now,
        });
        Ok(())
    }

    pub fn trigger_kill_switch(ctx: Context<TriggerKillSwitch>) -> Result<()> {
        let authority_key = ctx.accounts.authority.key();
        let safe_key = ctx.accounts.safe_wallet.key();
        let fee_bps = ctx.accounts.config.rescue_fee_bps;

        let vault_info = ctx.accounts.vault.to_account_info();
        let spendable = spendable_lamports(&vault_info)?;
        let fee = bps_of(spendable, fee_bps)?;
        let rest = spendable.checked_sub(fee).ok_or(ErrorCode::MathOverflow)?;

        move_lamports(&vault_info, &ctx.accounts.platform_treasury.to_account_info(), fee)?;
        move_lamports(&vault_info, &ctx.accounts.safe_wallet.to_account_info(), rest)?;

        let vault = &mut ctx.accounts.vault;
        vault.is_active = false;
        vault.swept_to = Some(safe_key);

        emit!(KillSwitchTriggered {
            vault: vault.key(),
            triggered_by: authority_key,
            safe_wallet: safe_key,
            amount_rescued: rest,
            fee,
        });
        Ok(())
    }

    pub fn claim_expired_vault(ctx: Context<ClaimExpiredVault>) -> Result<()> {
        let caller_key = ctx.accounts.caller.key();
        let backup_key = ctx.accounts.backup_wallet.key();
        let fee_bps = ctx.accounts.config.rescue_fee_bps;
        let now = Clock::get()?.unix_timestamp;

        let deadline = ctx
            .accounts
            .vault
            .last_check_in
            .checked_add(ctx.accounts.vault.timeout_seconds)
            .ok_or(ErrorCode::MathOverflow)?;
        require!(now > deadline, ErrorCode::VaultNotExpired);

        let vault_info = ctx.accounts.vault.to_account_info();
        let spendable = spendable_lamports(&vault_info)?;
        let fee = bps_of(spendable, fee_bps)?;
        let rest = spendable.checked_sub(fee).ok_or(ErrorCode::MathOverflow)?;

        move_lamports(&vault_info, &ctx.accounts.platform_treasury.to_account_info(), fee)?;
        move_lamports(&vault_info, &ctx.accounts.backup_wallet.to_account_info(), rest)?;

        let vault = &mut ctx.accounts.vault;
        vault.is_active = false;
        vault.swept_to = Some(backup_key);

        emit!(VaultClaimed {
            vault: vault.key(),
            claimed_by: caller_key,
            backup_wallet: backup_key,
            amount_rescued: rest,
            fee,
        });
        Ok(())
    }

    pub fn deposit_token(ctx: Context<DepositToken>, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::InvalidAmount);
        let depositor_key = ctx.accounts.depositor.key();
        let decimals = ctx.accounts.mint.decimals;

        token::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.depositor_token_account.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault_token_account.to_account_info(),
                    authority: ctx.accounts.depositor.to_account_info(),
                },
            ),
            amount,
            decimals,
        )?;

        let vault = &mut ctx.accounts.vault;
        if depositor_key == vault.owner {
            vault.last_check_in = Clock::get()?.unix_timestamp;
        }

        emit!(Deposited {
            vault: vault.key(),
            depositor: depositor_key,
            mint: Some(ctx.accounts.mint.key()),
            amount,
        });
        Ok(())
    }

    pub fn owner_withdraw_token(ctx: Context<OwnerWithdrawToken>, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::InvalidAmount);
        require!(
            ctx.accounts.vault_token_account.amount >= amount,
            ErrorCode::InsufficientVaultBalance
        );
        let owner_key = ctx.accounts.vault.owner;
        let id_bytes = ctx.accounts.vault.vault_id.to_le_bytes();
        let bump_arr = [ctx.accounts.vault.bump];
        let seeds: &[&[u8]] = &[b"vault", owner_key.as_ref(), &id_bytes, &bump_arr];
        let signer_seeds: &[&[&[u8]]] = &[seeds];
        let decimals = ctx.accounts.mint.decimals;

        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.vault_token_account.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.owner_token_account.to_account_info(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )?;

        let vault = &mut ctx.accounts.vault;
        vault.last_check_in = Clock::get()?.unix_timestamp;

        emit!(Withdrawn {
            vault: vault.key(),
            owner: owner_key,
            mint: Some(ctx.accounts.mint.key()),
            amount,
        });
        Ok(())
    }

    pub fn sweep_tokens(ctx: Context<SweepTokens>) -> Result<()> {
        let total = ctx.accounts.vault_token_account.amount;
        require!(total > 0, ErrorCode::InvalidAmount);
        let fee = bps_of(total, ctx.accounts.config.rescue_fee_bps)?;
        let rest = total.checked_sub(fee).ok_or(ErrorCode::MathOverflow)?;

        let owner_key = ctx.accounts.vault.owner;
        let id_bytes = ctx.accounts.vault.vault_id.to_le_bytes();
        let bump_arr = [ctx.accounts.vault.bump];
        let seeds: &[&[u8]] = &[b"vault", owner_key.as_ref(), &id_bytes, &bump_arr];
        let signer_seeds: &[&[&[u8]]] = &[seeds];
        let decimals = ctx.accounts.mint.decimals;

        if fee > 0 {
            token::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault_token_account.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.treasury_token_account.to_account_info(),
                        authority: ctx.accounts.vault.to_account_info(),
                    },
                    signer_seeds,
                ),
                fee,
                decimals,
            )?;
        }
        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.vault_token_account.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.recipient_token_account.to_account_info(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
                signer_seeds,
            ),
            rest,
            decimals,
        )?;

        emit!(TokensSwept {
            vault: ctx.accounts.vault.key(),
            mint: ctx.accounts.mint.key(),
            recipient: ctx.accounts.recipient_token_account.owner,
            amount: rest,
            fee,
            swept_by: ctx.accounts.caller.key(),
        });
        Ok(())
    }

    pub fn update_timeout(ctx: Context<UpdateTimeout>, new_timeout_seconds: i64) -> Result<()> {
        require!(
            (MIN_TIMEOUT..=MAX_TIMEOUT).contains(&new_timeout_seconds),
            ErrorCode::InvalidTimeout
        );
        let now = Clock::get()?.unix_timestamp;
        let vault = &mut ctx.accounts.vault;
        vault.timeout_seconds = new_timeout_seconds;
        vault.last_check_in = now;
        emit!(TimeoutUpdated {
            vault: vault.key(),
            owner: vault.owner,
            new_timeout_seconds,
            timestamp: now,
        });
        Ok(())
    }
}

fn system_transfer<'info>(
    system_program: &Program<'info, System>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    system_program::transfer(
        CpiContext::new(
            system_program.to_account_info(),
            system_program::Transfer { from, to },
        ),
        amount,
    )
}

fn effective_partner_bps(config: &Config, tier_info: &AccountInfo) -> Result<u16> {
    if tier_info.owner == &crate::ID && tier_info.data_len() >= 8 + PartnerTier::LEN {
        let data = tier_info.try_borrow_data()?;
        let tier = PartnerTier::try_deserialize(&mut &data[..])?;
        if tier.is_active {
            return Ok(tier.custom_bps);
        }
    }
    Ok(config.partner_bps)
}

fn split_fee(fee: u64, partner_bps: u16, has_partner: bool) -> Result<(u64, u64)> {
    let partner_share = if has_partner { bps_of(fee, partner_bps)? } else { 0 };
    let treasury_share = fee.checked_sub(partner_share).ok_or(ErrorCode::MathOverflow)?;
    Ok((partner_share, treasury_share))
}

fn bps_of(amount: u64, bps: u16) -> Result<u64> {
    let v = (amount as u128)
        .checked_mul(bps as u128)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ErrorCode::MathOverflow)?;
    u64::try_from(v).map_err(|_| error!(ErrorCode::MathOverflow))
}

fn spendable_lamports(info: &AccountInfo) -> Result<u64> {
    let rent_min = Rent::get()?.minimum_balance(info.data_len());
    Ok(info.lamports().saturating_sub(rent_min))
}

fn move_lamports(from: &AccountInfo, to: &AccountInfo, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let from_balance = from.lamports();
    let to_balance = to.lamports();
    **from.try_borrow_mut_lamports()? = from_balance
        .checked_sub(amount)
        .ok_or(ErrorCode::InsufficientVaultBalance)?;
    **to.try_borrow_mut_lamports()? = to_balance
        .checked_add(amount)
        .ok_or(ErrorCode::MathOverflow)?;
    Ok(())
}

fn process_renewal<'info>(
    config: &Config,
    vault: &mut Account<'info, Vault>,
    treasury: &AccountInfo<'info>,
    partner: &AccountInfo<'info>,
    now: i64,
    partner_bps: u16,
) -> Result<()> {
    if now <= vault.paid_until {
        return Ok(());
    }
    let fee = config.annual_fee_lamports;
    let vault_info = vault.to_account_info();
    let spendable = spendable_lamports(&vault_info)?;
    if spendable < fee {
        return Ok(());
    }

    let (partner_share, treasury_share) =
        split_fee(fee, partner_bps, vault.partner_wallet.is_some())?;
    move_lamports(&vault_info, partner, partner_share)?;
    move_lamports(&vault_info, treasury, treasury_share)?;

    let extended = vault
        .paid_until
        .checked_add(SECONDS_PER_YEAR)
        .ok_or(ErrorCode::MathOverflow)?;
    vault.paid_until = if extended < now {
        now.checked_add(SECONDS_PER_YEAR).ok_or(ErrorCode::MathOverflow)?
    } else {
        extended
    };

    emit!(RenewalProcessed {
        vault: vault.key(),
        fee,
        partner_share,
        treasury_share,
        paid_until: vault.paid_until,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(
        init,
        seeds = [b"config", authority.key().as_ref()],
        bump,
        payer = authority,
        space = 8 + Config::LEN
    )]
    pub config: Account<'info, Config>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(
        mut,
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
    )]
    pub config: Account<'info, Config>,

    pub admin: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(partner_wallet: Pubkey)]
pub struct SetPartnerTier<'info> {
    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + PartnerTier::LEN,
        seeds = [b"partner", partner_wallet.as_ref()],
        bump
    )]
    pub partner_tier: Account<'info, PartnerTier>,

    #[account(mut)]
    pub admin: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(partner_wallet: Pubkey)]
pub struct ClearPartnerTier<'info> {
    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"partner", partner_wallet.as_ref()],
        bump = partner_tier.bump,
        close = admin
    )]
    pub partner_tier: Account<'info, PartnerTier>,

    #[account(mut)]
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(vault_id: u64)]
pub struct CreateVault<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        init,
        seeds = [b"vault", owner.key().as_ref(), &vault_id.to_le_bytes()],
        bump,
        payer = owner,
        space = 8 + Vault::LEN
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,

    /// CHECK: Validated in handler against partner_wallet arg (or treasury); only receives lamports.
    #[account(mut)]
    pub partner: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: PDA derived from `partner`; read only if owned by this program.
    #[account(seeds = [b"partner", partner.key().as_ref()], bump)]
    pub partner_tier: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,

    /// CHECK: Must equal vault.partner_wallet or treasury; only receives lamports.
    #[account(mut, constraint = partner.key() == vault.partner_wallet.unwrap_or(config.platform_treasury) @ ErrorCode::InvalidFeeRecipient)]
    pub partner: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: PDA derived from `partner`; read only if owned by this program.
    #[account(seeds = [b"partner", partner.key().as_ref()], bump)]
    pub partner_tier: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct OwnerWithdraw<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = owner @ ErrorCode::Unauthorized,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,

    /// CHECK: Must equal vault.partner_wallet or treasury; only receives lamports.
    #[account(mut, constraint = partner.key() == vault.partner_wallet.unwrap_or(config.platform_treasury) @ ErrorCode::InvalidFeeRecipient)]
    pub partner: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    /// CHECK: PDA derived from `partner`; read only if owned by this program.
    #[account(seeds = [b"partner", partner.key().as_ref()], bump)]
    pub partner_tier: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CheckIn<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = owner @ ErrorCode::Unauthorized,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,

    /// CHECK: Must equal vault.partner_wallet or treasury; only receives lamports.
    #[account(mut, constraint = partner.key() == vault.partner_wallet.unwrap_or(config.platform_treasury) @ ErrorCode::InvalidFeeRecipient)]
    pub partner: UncheckedAccount<'info>,

    /// CHECK: PDA derived from `partner`; read only if owned by this program.
    #[account(seeds = [b"partner", partner.key().as_ref()], bump)]
    pub partner_tier: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct TriggerKillSwitch<'info> {
    #[account(
        mut,
        constraint = authority.key() == vault.owner || authority.key() == vault.safe_wallet @ ErrorCode::Unauthorized,
    )]
    pub authority: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Must equal vault.safe_wallet; only receives lamports.
    #[account(mut, constraint = safe_wallet.key() == vault.safe_wallet @ ErrorCode::InvalidWallet)]
    pub safe_wallet: UncheckedAccount<'info>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ClaimExpiredVault<'info> {
    pub caller: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    /// CHECK: Must equal vault.backup_wallet; only receives lamports.
    #[account(mut, constraint = backup_wallet.key() == vault.backup_wallet @ ErrorCode::InvalidWallet)]
    pub backup_wallet: UncheckedAccount<'info>,

    /// CHECK: Validated against config.platform_treasury; only receives lamports.
    #[account(mut, constraint = platform_treasury.key() == config.platform_treasury @ ErrorCode::InvalidFeeRecipient)]
    pub platform_treasury: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct DepositToken<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    pub mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = depositor,
    )]
    pub depositor_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = depositor,
        associated_token::mint = mint,
        associated_token::authority = vault,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct OwnerWithdrawToken<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = owner @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    pub mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = vault,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SweepTokens<'info> {
    pub caller: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = !vault.is_active @ ErrorCode::VaultStillActive,
        constraint = vault.swept_to.is_some() @ ErrorCode::VaultStillActive,
    )]
    pub vault: Box<Account<'info, Vault>>,

    pub mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = vault,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = mint,
        constraint = Some(recipient_token_account.owner) == vault.swept_to @ ErrorCode::InvalidWallet,
    )]
    pub recipient_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = mint,
        constraint = treasury_token_account.owner == config.platform_treasury @ ErrorCode::InvalidFeeRecipient,
    )]
    pub treasury_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct UpdateTimeout<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"config", config.authority.as_ref()],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, Config>>,

    #[account(
        mut,
        seeds = [b"vault", vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = owner @ ErrorCode::Unauthorized,
        has_one = config @ ErrorCode::Unauthorized,
        constraint = vault.is_active @ ErrorCode::VaultInactive,
    )]
    pub vault: Box<Account<'info, Vault>>,
}

#[account]
pub struct Config {
    pub admin: Pubkey,
    pub platform_treasury: Pubkey,
    pub annual_fee_lamports: u64,
    pub partner_bps: u16,
    pub rescue_fee_bps: u16,
    pub bump: u8,
    pub authority: Pubkey,
    pub is_active: bool,
    pub is_paused: bool,
    pub version: u8,
}

impl Config {
    pub const LEN: usize = 32 + 32 + 8 + 2 + 2 + 1 + 32 + 1 + 1 + 1;
}

#[account]
pub struct Vault {
    pub owner: Pubkey,
    pub backup_wallet: Pubkey,
    pub safe_wallet: Pubkey,
    pub vault_id: u64,
    pub partner_wallet: Option<Pubkey>,
    pub timeout_seconds: i64,
    pub last_check_in: i64,
    pub paid_until: i64,
    pub created_at: i64,
    pub is_active: bool,
    pub swept_to: Option<Pubkey>,
    pub bump: u8,
    pub config: Pubkey,
}

impl Vault {
    pub const LEN: usize = 32 + 32 + 32 + 8 + (1 + 32) + 8 + 8 + 8 + 8 + 1 + (1 + 32) + 1 + 32;
}

#[account]
pub struct PartnerTier {
    pub partner_wallet: Pubkey,
    pub custom_bps: u16,
    pub is_active: bool,
    pub bump: u8,
}

impl PartnerTier {
    pub const LEN: usize = 32 + 2 + 1 + 1;
}

#[event]
pub struct VaultCreated {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub vault_id: u64,
    pub backup_wallet: Pubkey,
    pub safe_wallet: Pubkey,
    pub partner_wallet: Option<Pubkey>,
    pub timeout_seconds: i64,
    pub fee_paid: u64,
    pub initial_deposit: u64,
    pub paid_until: i64,
}

#[event]
pub struct Deposited {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub mint: Option<Pubkey>,
    pub amount: u64,
}

#[event]
pub struct Withdrawn {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub mint: Option<Pubkey>,
    pub amount: u64,
}

#[event]
pub struct CheckedIn {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct RenewalProcessed {
    pub vault: Pubkey,
    pub fee: u64,
    pub partner_share: u64,
    pub treasury_share: u64,
    pub paid_until: i64,
}

#[event]
pub struct KillSwitchTriggered {
    pub vault: Pubkey,
    pub triggered_by: Pubkey,
    pub safe_wallet: Pubkey,
    pub amount_rescued: u64,
    pub fee: u64,
}

#[event]
pub struct VaultClaimed {
    pub vault: Pubkey,
    pub claimed_by: Pubkey,
    pub backup_wallet: Pubkey,
    pub amount_rescued: u64,
    pub fee: u64,
}

#[event]
pub struct TokensSwept {
    pub vault: Pubkey,
    pub mint: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
    pub fee: u64,
    pub swept_by: Pubkey,
}

#[event]
pub struct TimeoutUpdated {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub new_timeout_seconds: i64,
    pub timestamp: i64,
}

#[event]
pub struct PartnerTierSet {
    pub partner_wallet: Pubkey,
    pub custom_bps: u16,
    pub is_active: bool,
    pub admin: Pubkey,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Unauthorized access")]
    Unauthorized,
    #[msg("Vault is inactive")]
    VaultInactive,
    #[msg("Vault is still active")]
    VaultStillActive,
    #[msg("Vault has not expired yet")]
    VaultNotExpired,
    #[msg("Invalid timeout")]
    InvalidTimeout,
    #[msg("Invalid wallet")]
    InvalidWallet,
    #[msg("Invalid partner wallet")]
    InvalidPartner,
    #[msg("Insufficient vault balance")]
    InsufficientVaultBalance,
    #[msg("Invalid amount")]
    InvalidAmount,
    #[msg("Math overflow occurred")]
    MathOverflow,
    #[msg("Invalid fee recipient")]
    InvalidFeeRecipient,
    #[msg("Invalid basis points")]
    InvalidBps,
}

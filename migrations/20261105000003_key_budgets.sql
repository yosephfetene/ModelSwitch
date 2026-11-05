-- Optional monthly budgets (ADR-035). NULL means no budget, so existing
-- keys keep working. Spending is not stored here: it is the sum of the
-- key's `estimated_cost` this month, so there is no second counter to
-- drift. The upper bound also rejects NaN, which PostgreSQL sorts above
-- every number.

ALTER TABLE client_api_keys
    ADD COLUMN monthly_budget_usd numeric
        CHECK (monthly_budget_usd >= 0 AND monthly_budget_usd < 1000000);

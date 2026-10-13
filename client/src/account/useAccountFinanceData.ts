import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import { localizedError, stableErrorCode } from "../i18n";
import type { AccountFinanceApi } from "./accountApi";
import {
  createRefreshGate,
  type RefreshOptions,
} from "../refreshPolicy";
import type {
  BillingRecord,
  FinanceAvailability,
  PaymentOrder,
  PayoutAccount,
  Refund,
  UsageGift,
  Withdrawal,
} from "./accountTypes";

export type AccountFinanceData = {
  availability: FinanceAvailability;
  payments: PaymentOrder[];
  refunds: Refund[];
  payoutAccounts: PayoutAccount[];
  withdrawals: Withdrawal[];
  transfers: BillingRecord[];
  gifts: UsageGift[];
  giftableUsageBalance: number;
};

export type AccountFinanceDataController = {
  data: AccountFinanceData | null;
  loading: boolean;
  error: string;
  errorCode: string;
  setData: Dispatch<SetStateAction<AccountFinanceData | null>>;
  load(options?: RefreshOptions): Promise<AccountFinanceData | null>;
};

export function useAccountFinanceData(
  signedIn: boolean,
  financeApi: AccountFinanceApi,
): AccountFinanceDataController {
  const [data, setData] = useState<AccountFinanceData | null>(null);
  const dataRef = useRef<AccountFinanceData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [errorCode, setErrorCode] = useState("");
  const inFlight = useRef<Promise<AccountFinanceData | null> | null>(null);
  const activeRef = useRef(signedIn);
  const generation = useRef(0);
  const [refreshGate] = useState(createRefreshGate);

  activeRef.current = signedIn;

  const storeData = useCallback((next: SetStateAction<AccountFinanceData | null>) => {
    setData((current) => {
      const resolved = typeof next === "function" ? next(current) : next;
      dataRef.current = resolved;
      return resolved;
    });
  }, []);

  const load = useCallback(async (options: RefreshOptions = {}) => {
    if (!signedIn) return null;
    if (inFlight.current) {
      if (!options.force) return inFlight.current;
      await inFlight.current;
    }
    if (!refreshGate.begin(options)) return dataRef.current;

    setLoading(true);
    const loadGeneration = generation.current;
    const request = (async () => {
      try {
        const [availability, payments, refunds, payoutAccounts, withdrawals, transfers, giftList] = await Promise.all([
          financeApi.availability(),
          financeApi.paymentOrders(),
          financeApi.refunds(),
          financeApi.payoutAccounts(),
          financeApi.withdrawals(),
          financeApi.usageTransfers(),
          financeApi.usageGifts(),
        ]);
        const next: AccountFinanceData = {
          availability,
          payments,
          refunds,
          payoutAccounts,
          withdrawals,
          transfers,
          gifts: giftList.data,
          giftableUsageBalance: giftList.giftable_usage_balance,
        };
        if (!activeRef.current || generation.current !== loadGeneration) return null;
        storeData(next);
        setError("");
        setErrorCode("");
        return next;
      } catch (loadError) {
        if (activeRef.current && generation.current === loadGeneration) {
          refreshGate.reset();
          setError(localizedError(loadError));
          setErrorCode(stableErrorCode(loadError));
        }
        return null;
      }
    })();
    inFlight.current = request;
    try {
      return await request;
    } finally {
      if (inFlight.current === request) {
        inFlight.current = null;
        setLoading(false);
      }
    }
  }, [financeApi, refreshGate, signedIn, storeData]);

  useEffect(() => {
    if (signedIn) return;
    generation.current += 1;
    inFlight.current = null;
    refreshGate.reset();
    dataRef.current = null;
    setData(null);
    setLoading(false);
    setError("");
    setErrorCode("");
  }, [refreshGate, signedIn]);

  return { data, loading, error, errorCode, setData: storeData, load };
}

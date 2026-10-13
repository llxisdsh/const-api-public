import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { invoke } from "@tauri-apps/api/core";

import { stableErrorCode } from "../i18n";
import {
  createRefreshGate,
  type RefreshOptions,
} from "../refreshPolicy";
import type { ReferralOverview } from "./accountTypes";

export type AccountReferralDataController = {
  data: ReferralOverview | null;
  loading: boolean;
  error: string;
  errorCode: string;
  setData: Dispatch<SetStateAction<ReferralOverview | null>>;
  load(options?: RefreshOptions): Promise<ReferralOverview | null>;
};

export function useAccountReferralData(signedIn: boolean): AccountReferralDataController {
  const [data, setData] = useState<ReferralOverview | null>(null);
  const dataRef = useRef<ReferralOverview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [errorCode, setErrorCode] = useState("");
  const inFlight = useRef<Promise<ReferralOverview | null> | null>(null);
  const activeRef = useRef(signedIn);
  const generation = useRef(0);
  const [refreshGate] = useState(createRefreshGate);

  activeRef.current = signedIn;

  const storeData = useCallback((next: SetStateAction<ReferralOverview | null>) => {
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
    if ((options.throttle || options.force) && !refreshGate.begin(options)) {
      return dataRef.current;
    }

    setLoading(true);
    const loadGeneration = generation.current;
    const request = (async () => {
      try {
        const next = await invoke<ReferralOverview>("account_referral_overview");
        if (!activeRef.current || generation.current !== loadGeneration) return null;
        storeData(next);
        setError("");
        setErrorCode("");
        return next;
      } catch (loadError) {
        if (activeRef.current && generation.current === loadGeneration) {
          refreshGate.reset();
          setError(loadError instanceof Error ? loadError.message : String(loadError));
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
  }, [refreshGate, signedIn, storeData]);

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

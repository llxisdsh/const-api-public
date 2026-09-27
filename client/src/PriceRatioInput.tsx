import { useEffect, useRef, useState, type FocusEvent } from "react";

type PriceRatioInputProps = {
  value: number;
  onValueChange: (value: number) => void;
  max: number;
  className?: string;
  disabled?: boolean;
  id?: string;
  name?: string;
  "aria-label"?: string;
};

function displayRatio(value: number, max: number) {
  const normalized = Number.isFinite(value) ? Math.min(max, Math.max(0, value)) : 0;
  return normalized.toFixed(12).replace(/\.?0+$/, "");
}

function parsedDraft(value: string) {
  const normalized = value.trim().replace(",", ".");
  if (!/^(?:\d+(?:\.\d*)?|\.\d+)$/.test(normalized)) return null;
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : null;
}

function isEditableDraft(value: string) {
  const normalized = value.replace(",", ".");
  return normalized === "" || /^(?:\d+(?:\.\d*)?|\.\d*)$/.test(normalized);
}

export function PriceRatioInput({
  value,
  onValueChange,
  max,
  className,
  disabled,
  id,
  name,
  "aria-label": ariaLabel,
}: PriceRatioInputProps) {
  const focused = useRef(false);
  const [draft, setDraft] = useState(() => displayRatio(value, max));

  useEffect(() => {
    if (!focused.current) setDraft(displayRatio(value, max));
  }, [max, value]);

  const commit = (event: FocusEvent<HTMLInputElement>) => {
    focused.current = false;
    const parsed = parsedDraft(event.currentTarget.value);
    if (parsed === null) {
      setDraft(displayRatio(value, max));
      return;
    }

    const normalized = Math.min(max, Math.max(0, parsed));
    setDraft(displayRatio(normalized, max));
    if (normalized !== value) onValueChange(normalized);
  };

  return (
    <input
      id={id}
      name={name}
      className={className}
      type="text"
      inputMode="decimal"
      autoComplete="off"
      spellCheck={false}
      disabled={disabled}
      aria-label={ariaLabel}
      value={draft}
      onFocus={() => {
        focused.current = true;
      }}
      onChange={(event) => {
        const next = event.currentTarget.value;
        if (!isEditableDraft(next)) return;
        setDraft(next);
        const parsed = parsedDraft(next);
        if (parsed !== null && parsed >= 0 && parsed <= max && parsed !== value) {
          onValueChange(parsed);
        }
      }}
      onBlur={commit}
    />
  );
}

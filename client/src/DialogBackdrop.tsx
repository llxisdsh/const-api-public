import type { ReactNode } from "react";

type DialogBackdropProps = {
  "aria-describedby"?: string;
  "aria-label"?: string;
  "aria-labelledby"?: string;
  children: ReactNode;
  className?: string;
  variant?: "drawer" | "modal";
};

export function DialogBackdrop({
  "aria-describedby": ariaDescribedBy,
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  children,
  className = "",
  variant = "modal",
}: DialogBackdropProps) {
  const backdropClassName = `${variant}-backdrop${className ? ` ${className}` : ""}`;

  return (
    <div
      aria-describedby={ariaDescribedBy}
      aria-label={ariaLabel}
      aria-labelledby={ariaLabelledBy}
      aria-modal="true"
      className={backdropClassName}
      data-backdrop-dismissible="false"
      role="dialog"
    >
      {children}
    </div>
  );
}

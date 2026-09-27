import { X } from "lucide-react";
import { useTranslation } from "react-i18next";

export function ModalCloseButton({
  onClick,
  label,
  disabled = false,
}: {
  onClick: () => void;
  label?: string;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const accessibleLabel = label ?? t("common.close");
  return (
    <button
      className="modal-close-button"
      type="button"
      aria-label={accessibleLabel}
      title={accessibleLabel}
      disabled={disabled}
      onClick={onClick}
    >
      <X size={18} aria-hidden="true" />
    </button>
  );
}

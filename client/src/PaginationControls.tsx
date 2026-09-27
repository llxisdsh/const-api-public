import { ChevronLeft, ChevronRight } from "lucide-react";
import type { ReactNode } from "react";

type PaginationControlsProps = {
  ariaLabel: string;
  busy?: boolean;
  canGoNext: boolean;
  canGoPrevious: boolean;
  currentPage: number;
  currentPageLabel: string;
  nextLabel: string;
  onNext: () => void;
  onPrevious: () => void;
  previousLabel: string;
  summary?: ReactNode;
};

export function PaginationControls({
  ariaLabel,
  busy = false,
  canGoNext,
  canGoPrevious,
  currentPage,
  currentPageLabel,
  nextLabel,
  onNext,
  onPrevious,
  previousLabel,
  summary,
}: PaginationControlsProps) {
  return (
    <nav className="pagination-controls" aria-label={ariaLabel}>
      {summary !== undefined && <span className="pagination-controls-summary">{summary}</span>}
      <div className="pagination-controls-actions">
        <button
          className="pagination-controls-button"
          type="button"
          aria-label={previousLabel}
          title={previousLabel}
          disabled={busy || !canGoPrevious}
          onClick={onPrevious}
        >
          <ChevronLeft className="pagination-controls-icon" aria-hidden="true" size={20} strokeWidth={1.9} />
        </button>
        <span
          className="pagination-controls-page"
          aria-label={currentPageLabel}
          aria-live="polite"
        >
          {currentPage}
        </span>
        <button
          className="pagination-controls-button"
          type="button"
          aria-label={nextLabel}
          title={nextLabel}
          disabled={busy || !canGoNext}
          onClick={onNext}
        >
          <ChevronRight className="pagination-controls-icon" aria-hidden="true" size={20} strokeWidth={1.9} />
        </button>
      </div>
    </nav>
  );
}

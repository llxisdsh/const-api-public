import { Component, type ErrorInfo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

type AppErrorBoundaryProps = {
  children: ReactNode;
  fallback: ReactNode;
};

type AppErrorBoundaryState = {
  failed: boolean;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[const-api][renderer] recovered render error", error, info.componentStack);
  }

  render() {
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}

export function RendererFailure({ onReload = () => window.location.reload() }: { onReload?: () => void }) {
  const { t } = useTranslation();
  return (
    <section className="renderer-failure" role="alert" aria-labelledby="renderer-failure-title">
      <div className="renderer-failure-card">
        <span className="renderer-failure-brand">CONST API</span>
        <h1 id="renderer-failure-title">{t("rendererFailure.title")}</h1>
        <p>{t("rendererFailure.description")}</p>
        <button type="button" onClick={onReload}>{t("rendererFailure.reload")}</button>
      </div>
    </section>
  );
}

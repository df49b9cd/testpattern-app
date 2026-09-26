import { Component, type ErrorInfo, type ReactNode } from "react";
import { TriangleAlert } from "lucide-react";
import { Button } from "./ui";

/** Keeps a rendering bug from blanking the whole window. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey?: string }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("UI error:", error, info.componentStack);
  }

  componentDidUpdate(prev: { resetKey?: string }) {
    // navigating elsewhere clears the error
    if (this.state.error && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="grid h-full place-items-center bg-bg p-8">
        <div className="flex max-w-md flex-col items-center gap-4 text-center">
          <div className="grid size-14 place-items-center rounded-2xl bg-live/15 text-live">
            <TriangleAlert className="size-7" />
          </div>
          <h2 className="text-xl font-bold">Something went wrong</h2>
          <p className="text-sm text-dim">{this.state.error.message}</p>
          <div className="flex gap-3">
            <Button variant="primary" onClick={() => this.setState({ error: null })}>
              Try again
            </Button>
            <Button onClick={() => window.location.reload()}>Reload app</Button>
          </div>
        </div>
      </div>
    );
  }
}

import { describe, expect, it } from "vitest";
import { anySyncRunning, useSync } from "./sync";

describe("sync progress", () => {
  it("follows a sync from start to finish", () => {
    const { apply } = useSync.getState();
    apply({ stage: "started", sourceId: 7 });
    expect(useSync.getState().bySource[7]).toMatchObject({ running: true, error: null });
    expect(anySyncRunning(useSync.getState())).toBe(true);
    apply({ stage: "step", sourceId: 7, message: "Saving…" });
    expect(useSync.getState().bySource[7].message).toBe("Saving…");
    apply({ stage: "done", sourceId: 7, counts: { channels: 1, movies: 0, series: 0, programmes: 0 } });
    expect(useSync.getState().bySource[7]).toMatchObject({ running: false, message: "Up to date" });
    expect(anySyncRunning(useSync.getState())).toBe(false);
  });
  it("keeps the error of a failed sync until the next one starts", () => {
    const { apply } = useSync.getState();
    apply({ stage: "failed", sourceId: 8, error: "Login failed" });
    expect(useSync.getState().bySource[8]).toMatchObject({ running: false, error: "Login failed" });
    apply({ stage: "started", sourceId: 8 });
    expect(useSync.getState().bySource[8].error).toBeNull();
  });
});

import { QueryClient } from "@tanstack/react-query";
import { queryKeys } from "./queryKeys";

/** The app's TanStack Query cache (also used by stores outside React). */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 60_000,
      gcTime: 10 * 60_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});

/** Views that show watch progress: refetch after it changed. */
export function invalidateWatchState() {
  for (const queryKey of [queryKeys.continue, queryKeys.workDetail("movie"), queryKeys.workDetail("series"), queryKeys.works("movie")]) {
    void queryClient.invalidateQueries({ queryKey });
  }
}

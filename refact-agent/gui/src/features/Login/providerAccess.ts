import type { ProviderListItem } from "../../services/refact";

export function hasAnyUsableActiveProvider({
  providers,
}: {
  providers: ProviderListItem[];
}): boolean {
  return providers.some((provider) => provider.status === "active");
}

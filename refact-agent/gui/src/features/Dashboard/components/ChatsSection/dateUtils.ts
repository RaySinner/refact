export function getDateGroup(dateStr: string): string {
  const date = new Date(dateStr);
  const now = new Date();
  const todayUTC = Date.UTC(now.getFullYear(), now.getMonth(), now.getDate());
  const dateUTC = Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
  const diffDay = Math.floor((todayUTC - dateUTC) / 86_400_000);

  if (diffDay === 0) return "Today";
  if (diffDay === 1) return "Yesterday";
  return "Earlier";
}

export function keepPreviousIfStructurallyEqual<T>(previous: T, next: T): T {
  if (Object.is(previous, next)) return previous;
  try {
    return JSON.stringify(previous) === JSON.stringify(next) ? previous : next;
  } catch {
    return next;
  }
}

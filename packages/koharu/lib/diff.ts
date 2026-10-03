export type Piece = 'same' | 'removed' | 'added'

/**
 * Word by word difference between a balloon's current line and a proposed
 * one (longest common subsequence; balloons are short). Consecutive words of
 * the same kind are joined.
 */
export function wordDiff(before: string, after: string): [Piece, string][] {
  const a = before.split(/\s+/).filter(Boolean)
  const b = after.split(/\s+/).filter(Boolean)
  const common = Array.from({ length: a.length + 1 }, () =>
    Array.from({ length: b.length + 1 }, () => 0),
  )
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      common[i][j] =
        a[i] === b[j] ? common[i + 1][j + 1] + 1 : Math.max(common[i + 1][j], common[i][j + 1])
    }
  }
  const pieces: [Piece, string][] = []
  const push = (piece: Piece, word: string) => {
    const last = pieces.at(-1)
    if (last && last[0] === piece) last[1] += ` ${word}`
    else pieces.push([piece, word])
  }
  let i = 0
  let j = 0
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      push('same', a[i])
      i++
      j++
    } else if (i < a.length && (j === b.length || common[i + 1][j] >= common[i][j + 1])) {
      push('removed', a[i])
      i++
    } else {
      push('added', b[j])
      j++
    }
  }
  return pieces
}

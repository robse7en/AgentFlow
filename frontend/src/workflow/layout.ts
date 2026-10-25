const xArchitect = 22.694802319008915
const yArchitect = 322.913681842131

const xRequirements = 384.2764785325484
const yRequirements = 524.0060248940073

const xImplementer = 741.0598877158116
const yImplementer = 301.26821370750133

const xCodeReview = 1107.336411921101
const yCodeReview = 131.0941636045339

const xSecurityReview = 1111.223182371668
const ySecurityReview = 454.65434474338804

const xConsolidate = 1484.0960604425254
const yConsolidate = 345.65569347004856

const xFinalArchitect = 1845.8014031300595
const yFinalArchitect = 445.8445763626552

const xReadyForPr = 2155.5099838100377
const yReadyForPr = 377.3610361575823

export const NODE_POSITIONS: Record<string, { x: number; y: number }> = {
  orchestrator: { x: xImplementer, y: yImplementer + 400 },
  architect: { x: xArchitect, y: yArchitect },
  requirements_gate: { x: xRequirements, y: yRequirements },
  implementer: { x: xImplementer, y: yImplementer },
  code_review: { x: xCodeReview, y: yCodeReview },
  security_review: { x: xSecurityReview, y: ySecurityReview },
  consolidate: { x: xConsolidate, y: yConsolidate },
  final_architect: { x: xFinalArchitect, y: yFinalArchitect },
  ready_for_pr: { x: xReadyForPr, y: yReadyForPr },
}

// Height of the horizontal run of the consolidate->implementer loop-back edge. Routed above
// Code Review (the topmost branch node) so it doesn't cut through either branch.
export const LOOPBACK_Y = NODE_POSITIONS.code_review.y - 60

export const EDGE_LABELS: Record<string, string> = {
  'consolidate->implementer': 'blocking findings → fix',
  'consolidate->final_architect': 'reviews passed',
}

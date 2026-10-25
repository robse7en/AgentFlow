import { reactive } from 'vue';

// Session-only acknowledgement, scoped to the detected executable and selected harness.
// This records the user's confirmation; it is not an authentication test.
export const acknowledgedCliSignIns = reactive(new Set<string>());

import { createSecurityHandle } from 'specra/middleware/security';
import { sequence } from '@sveltejs/kit/hooks';

export const handle = sequence(
  createSecurityHandle({
    strictPathValidation: true,
  })
);

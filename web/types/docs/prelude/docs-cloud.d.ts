// What the Worker on cloud.html takes from the page it goes on from: the
// `Tenant` class of serverless.html, whose `export default` it replaces,
// and that class's `Env` with the token the Worker asks for beside it. A
// module, since the class extends one imported; what it declares is global.

import { DurableObject } from 'cloudflare:workers';

declare global {
  class Tenant extends DurableObject<Env> {
    query(sql: string, params: unknown[]): Promise<unknown>;
  }
  interface Env {
    TENANT: DurableObjectNamespace<Tenant>;
    FENNEC_TOKEN: string;
  }
}

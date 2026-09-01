import { Context, Service } from "@deepseek-ai/cordis";
import type { CallTracker, EventSource, ParentLink } from "./parent.js";

const UUID = { type: "string" } as const;

// dsh-tools 0.1.0-rc.6 refuses register() without a canonical output
// declaration. Product tools return one parent-authored text payload.
const PRODUCT_OUTPUT = {
  schema: {
    type: "object",
    additionalProperties: false,
    required: ["text"],
    properties: {
      text: { type: "string" },
    },
  },
  render(_args: unknown, value: unknown) {
    const text =
      value !== null &&
      typeof value === "object" &&
      "text" in value &&
      typeof (value as { text: unknown }).text === "string"
        ? (value as { text: string }).text
        : "";
    return [{ type: "text" as const, text }];
  },
};

const PRODUCT_TOOLS: ReadonlyArray<{
  name: string;
  description: string;
  parameters: Record<string, unknown>;
}> = [
  { name: "application.create", description: "Create or attach an Application on the current Workspace. Call this to start the software project the user asked you to build; then write voie.toml and source with bash under /workspace.", parameters: { type: "object", properties: { name: { type: "string" }, slug: { type: "string" } }, required: ["name", "slug"], additionalProperties: true } },
  { name: "application.inspect", description: "Inspect the Application bound to this Workspace.", parameters: { type: "object", additionalProperties: true } },
  { name: "application.status", description: "Show Application Environments, Releases, Deployments, Databases, and pending approvals. Poll this after release.build or deploy until the Release is ready and the candidate Deployment is healthy, then call deployment.activate.", parameters: { type: "object", additionalProperties: true } },
  { name: "application.suspend", description: "Suspend the Application: stop Deployments without deleting Databases or Workspace volumes.", parameters: { type: "object", additionalProperties: true } },
  { name: "application.archive", description: "Archive the Application: keep Blob restore points and release local Workspace, Database, and Deployment volumes. Distinct from suspend (keeps volumes) and delete (no final backup).", parameters: { type: "object", additionalProperties: true } },
  { name: "application.restore", description: "Restore an archived Application from pinned Blob Workspace snapshot and Database backups onto candidate LVs. Distinct from suspend.", parameters: { type: "object", additionalProperties: true } },
  { name: "application.delete", description: "Delete the Application after delete_application approval. Stops Deployments, Databases, routes, and Fabric journal rows.", parameters: { type: "object", properties: { approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "release.build", description: "Pack the Workspace guest voie.toml and source into an immutable Release. Reads voie.toml from the guest. Resources above the default tier require increase_resource_tier approval.", parameters: { type: "object", properties: { build_intent_id: UUID, buildIntentId: UUID, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "release.inspect", description: "Inspect one Release.", parameters: { type: "object", properties: { release_id: UUID, releaseId: UUID }, additionalProperties: true } },
  { name: "environment.deploy_dev", description: "Materialize a ready Release in private dev. Omitting release_id uses the latest ready Release. Call database.create first and wait until database.status is ready when the Release declares postgres. Does not switch traffic; after healthy, call deployment.activate.", parameters: { type: "object", properties: { release_id: UUID, releaseId: UUID, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "environment.set_visibility", description: "Set development Environment visibility.", parameters: { type: "object", properties: { kind: { type: "string" }, visibility: { type: "string" }, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "environment.publish_prod", description: "Materialize an existing Release in production after human approval (approval_id). Omitting release_id uses the latest ready Release. Does not rebuild or switch traffic. Call database.create for prod and wait until ready when the Release declares postgres. After healthy, call deployment.activate.", parameters: { type: "object", properties: { release_id: UUID, releaseId: UUID, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "deployment.status", description: "Show Deployment state. Omitting deployment_id lists Deployments for this Application.", parameters: { type: "object", properties: { deployment_id: UUID, deploymentId: UUID }, additionalProperties: true } },
  { name: "deployment.activate", description: "Switch Environment traffic to a healthy Deployment. Omitting deployment_id uses the latest healthy Deployment. Required after deploy_dev or publish_prod. Production requires ManageProduction.", parameters: { type: "object", properties: { deployment_id: UUID, deploymentId: UUID }, additionalProperties: true } },
  { name: "deployment.rollback", description: "Create a new Deployment of the previous Release. Does not mutate the old row back to active.", parameters: { type: "object", properties: { deployment_id: UUID, deploymentId: UUID, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "deployment.restart", description: "Recreate the same Deployment Pod without changing the Release.", parameters: { type: "object", properties: { deployment_id: UUID, deploymentId: UUID }, additionalProperties: true } },
  { name: "deployment.logs", description: "List Deployment log chunk metadata.", parameters: { type: "object", properties: { deployment_id: UUID, deploymentId: UUID }, additionalProperties: true } },
  { name: "database.create", description: "Create the dedicated PostgreSQL Database for one Environment kind. Call before deploying a Release that declares postgres. Poll database.status until ready.", parameters: { type: "object", properties: { kind: { type: "string" } }, required: ["kind"], additionalProperties: true } },
  { name: "database.status", description: "Show Database state. Omitting database_id lists Databases for this Application. Optional kind selects one Environment. Wait for ready before deploy_dev or publish_prod when the Release uses postgres.", parameters: { type: "object", properties: { database_id: UUID, databaseId: UUID, kind: { type: "string" } }, additionalProperties: true } },
  { name: "database.backup", description: "Dispatch a manual Database backup. The dump is a Blob object; credentials never enter the result.", parameters: { type: "object", properties: { database_id: UUID, databaseId: UUID }, additionalProperties: true } },
  { name: "database.list_backups", description: "List Database backup metadata without dump bytes or credentials.", parameters: { type: "object", properties: { database_id: UUID, databaseId: UUID }, additionalProperties: true } },
  { name: "database.restore", description: "Restore one backup into the Database after restore_database approval. Always allocates a candidate LV and switches only after proof.", parameters: { type: "object", properties: { database_id: UUID, databaseId: UUID, backup_id: UUID, backupId: UUID, approval_id: UUID, approvalId: UUID }, additionalProperties: true } },
  { name: "workspace.snapshot", description: "Capture a Blob Workspace snapshot including .git. Distinct from a Release pack. Retention drops unpinned snapshots beyond the platform bound.", parameters: { type: "object", additionalProperties: true } },
  { name: "secret.list_metadata", description: "List Project secret metadata without values.", parameters: { type: "object", additionalProperties: true } },
  { name: "secret.request_binding", description: "Request that a named secret be bound to an Environment.", parameters: { type: "object", properties: { kind: { type: "string" }, name: { type: "string" }, secret_id: UUID, secretId: UUID }, additionalProperties: true } },
];

export interface ProductToolDeps {
  parent: ParentLink;
  events: EventSource;
  calls: CallTracker;
}

/**
 * Registers typed Application-platform tools that execute only on the parent.
 * The child never receives Fabric, Blob, or secret material.
 */
export default class ParentProductTools extends Service {
  static readonly inject = ["tools"];

  constructor(ctx: Context, deps: ProductToolDeps) {
    super(ctx, "voie.product-tools");
    const tools = (ctx as Context & { tools?: { register?(tool: unknown): void; define?(tool: unknown): void } }).tools;
    const register = tools?.register ?? tools?.define;
    if (typeof register !== "function") {
      throw new Error("DSH tool runtime is not installed");
    }
    for (const spec of PRODUCT_TOOLS) {
      register.call(tools, {
        name: spec.name,
        description: spec.description,
        parameters: spec.parameters,
        output: PRODUCT_OUTPUT,
        async execute(args: Record<string, unknown>) {
          const call_id = deps.calls.take();
          if (call_id === undefined) {
            throw new Error("product intent has no outstanding model call id");
          }
          const reply = await deps.parent.product({
            call_id,
            name: spec.name,
            arguments: args,
            events: deps.events.collect(),
          });
          deps.events.advance();
          if (reply.is_error) {
            throw new Error(reply.text || `${spec.name} failed`);
          }
          return { text: reply.text };
        },
      });
    }
  }
}

# Deploying playmmix

playmmix is served from CloudFront over an S3 origin at
[playmmix.2ad.com](https://playmmix.2ad.com). This stack provisions the
certificate, bucket, distribution, and DNS records. Pushing a tag runs
`.github/workflows/deploy-static-site.yml`, which builds the bundle with
Trunk, deploys this stack, syncs the bundle to the bucket, and invalidates
the CloudFront cache.

A tag deploys the stack as it stands at the tagged commit. Before tagging a
release that carries stack changes, review them with AWS credentials:
`bun run cdk diff StackPlaymmix2adCom --no-telemetry`. The workflow deploys
with `--no-telemetry`, which also drops the `AWS::CDK::Metadata` resource, so
the local diff passes it too.

One step is human-only: take the `DistributionId` output from a deploy and
set it as the `CLOUDFRONT_DISTRIBUTION_ID` secret in this repository. Set it
again whenever the stack is recreated: a destroy and redeploy creates a new
distribution, and a stale id fails the invalidation step. Until the secret is
set, the invalidation step is skipped.

## Lifecycle

The stack owns every resource it needs, including its S3 origin bucket, so it
can be destroyed and redeployed freely with `bun run cdk:destroy` and
`bun run cdk:deploy StackPlaymmix2adCom`. Destroying it **deletes the bucket
and its contents** — safe, because the bundle is nothing but a Trunk build
artifact: `trunk build` regenerates it from source, and the next tag push
redeploys the stack and re-syncs the bundle. The one thing
`cdk destroy` does not touch is the hosted zone, `Z09862671HYH6ZFKNPGNL`,
which this stack imports read-only and no stack owns.

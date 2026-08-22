# Deploying playmmix

playmmix is served from CloudFront over an S3 origin at
[playmmix.2ad.com](https://playmmix.2ad.com). This stack provisions the
certificate, bucket, distribution, and DNS records. Pushing a tag runs
`.github/workflows/deploy-static-site.yml`, which builds the bundle with
Trunk and syncs it to the bucket.

Two steps are human-only and are not run by the workflow:

1. Deploy the stack with AWS credentials: `bun run cdk:deploy StackPlaymmix2adCom`.
2. Take the `DistributionId` output from that deploy and set it as the
   `CLOUDFRONT_DISTRIBUTION_ID` secret in this repository. Until that secret
   is set, the deploy workflow's cache-invalidation step is skipped.

## Lifecycle

The stack owns every resource it needs, including its S3 origin bucket, so it
can be destroyed and redeployed freely with `bun run cdk:destroy` and
`bun run cdk:deploy StackPlaymmix2adCom`. Destroying it **deletes the bucket
and its contents** — safe, because the bundle is nothing but a Trunk build
artifact: `trunk build` regenerates it from source and
`deploy-static-site.yml` re-syncs it on the next tag push. The one thing
`cdk destroy` does not touch is the hosted zone, `Z09862671HYH6ZFKNPGNL`,
which this stack imports read-only and no stack owns.

import * as cdk from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';

import { PlaymmixStack } from './playmmix-stack';

describe('PlaymmixStack', () => {
  const app = new cdk.App();
  const stack = new PlaymmixStack(app, 'TestPlaymmixStack', {
    env: {
      account: '504242000181',
      region: 'us-east-1',
    },
  });

  const template = Template.fromStack(stack);

  it('creates exactly one of each core resource, including an owned bucket', () => {
    template.resourceCountIs('AWS::CloudFront::Distribution', 1);
    template.resourceCountIs('AWS::CertificateManager::Certificate', 1);
    template.resourceCountIs('AWS::S3::BucketPolicy', 1);
    // Owned bucket, provisioned by this stack rather than imported — a
    // regression to an imported bucket would drop this resource entirely.
    template.resourceCountIs('AWS::S3::Bucket', 1);
  });

  it('secures the origin with Origin Access Control, not legacy OAI', () => {
    const oacResources = template.findResources('AWS::CloudFront::OriginAccessControl');
    const oacLogicalIds = Object.keys(oacResources);
    expect(oacLogicalIds).toHaveLength(1);

    template.hasResourceProperties('AWS::CloudFront::Distribution', {
      DistributionConfig: Match.objectLike({
        Origins: Match.arrayWith([
          Match.objectLike({
            OriginAccessControlId: {
              'Fn::GetAtt': [oacLogicalIds[0], 'Id'],
            },
          }),
        ]),
      }),
    });

    template.resourceCountIs('AWS::CloudFront::CloudFrontOriginAccessIdentity', 0);
  });

  it('destroys the bucket, encrypts it, and blocks all public access', () => {
    template.hasResource('AWS::S3::Bucket', {
      DeletionPolicy: 'Delete',
      UpdateReplacePolicy: 'Delete',
      Properties: Match.objectLike({
        BucketName: 'playmmix-us-east-1-504242000181',
        BucketEncryption: Match.anyValue(),
        PublicAccessBlockConfiguration: {
          BlockPublicAcls: true,
          BlockPublicPolicy: true,
          IgnorePublicAcls: true,
          RestrictPublicBuckets: true,
        },
      }),
    });
  });

  it('empties the bucket before teardown via the auto-delete custom resource', () => {
    const bucketLogicalIds = Object.keys(template.findResources('AWS::S3::Bucket'));
    expect(bucketLogicalIds).toHaveLength(1);
    const [bucketLogicalId] = bucketLogicalIds;

    const handlerRoleLogicalIds = Object.keys(template.findResources('AWS::IAM::Role'));
    expect(handlerRoleLogicalIds).toHaveLength(1);
    const [handlerRoleLogicalId] = handlerRoleLogicalIds;

    // BucketName must reference this stack's own bucket, not merely exist —
    // a custom resource pointed at a different bucket would pass a bare
    // resource-count check while leaving this bucket un-emptied.
    template.hasResourceProperties('Custom::S3AutoDeleteObjects', {
      BucketName: { Ref: bucketLogicalId },
    });

    // The handler role needs an explicit delete grant on this bucket, or
    // the custom resource exists but fails at runtime with AccessDenied.
    template.hasResourceProperties('AWS::S3::BucketPolicy', {
      Bucket: { Ref: bucketLogicalId },
      PolicyDocument: {
        Statement: Match.arrayWith([
          Match.objectLike({
            Effect: 'Allow',
            Action: Match.arrayWith(['s3:DeleteObject*']),
            Principal: {
              AWS: {
                'Fn::GetAtt': [handlerRoleLogicalId, 'Arn'],
              },
            },
          }),
        ]),
      },
    });
  });

  it('denies non-TLS requests via the enforceSSL statement', () => {
    template.hasResourceProperties('AWS::S3::BucketPolicy', {
      PolicyDocument: {
        Statement: Match.arrayWith([
          Match.objectLike({
            Effect: 'Deny',
            Principal: { AWS: '*' },
            Condition: {
              Bool: { 'aws:SecureTransport': 'false' },
            },
          }),
        ]),
      },
    });
  });

  it('creates A and AAAA alias records for playmmix.2ad.com', () => {
    template.hasResourceProperties('AWS::Route53::RecordSet', {
      Name: 'playmmix.2ad.com.',
      Type: 'A',
    });

    template.hasResourceProperties('AWS::Route53::RecordSet', {
      Name: 'playmmix.2ad.com.',
      Type: 'AAAA',
    });
  });

  it('serves playmmix.2ad.com with index.html as the root object', () => {
    template.hasResourceProperties('AWS::CloudFront::Distribution', {
      DistributionConfig: Match.objectLike({
        Aliases: Match.arrayWith(['playmmix.2ad.com']),
        DefaultRootObject: 'index.html',
      }),
    });
  });

  it('returns a real 404 for a missing asset, with no 403 fallback', () => {
    template.hasResourceProperties('AWS::CloudFront::Distribution', {
      DistributionConfig: Match.objectLike({
        CustomErrorResponses: [
          Match.objectLike({
            ErrorCode: 404,
            ResponseCode: 404,
            ResponsePagePath: '/index.html',
          }),
        ],
      }),
    });
  });

  it('scopes the bucket policy to CloudFront by Sid, with the expected effects', () => {
    template.hasResourceProperties('AWS::S3::BucketPolicy', {
      Bucket: { Ref: Match.anyValue() },
      PolicyDocument: {
        Statement: Match.arrayWith([
          Match.objectLike({
            Sid: 'AllowCloudFrontServicePrincipalReadOnly',
            Effect: 'Allow',
            Action: 's3:GetObject',
          }),
          Match.objectLike({
            Sid: 'DenyDirectS3ReadForObjects',
            Effect: 'Deny',
            Action: 's3:GetObject',
          }),
        ]),
      },
    });
  });

  it('grants CloudFront ListBucket on the bucket itself, so a missing key is a 404', () => {
    const bucketLogicalIds = Object.keys(template.findResources('AWS::S3::Bucket'));
    expect(bucketLogicalIds).toHaveLength(1);
    const [bucketLogicalId] = bucketLogicalIds;

    const distributionLogicalIds = Object.keys(template.findResources('AWS::CloudFront::Distribution'));
    expect(distributionLogicalIds).toHaveLength(1);
    const [distributionLogicalId] = distributionLogicalIds;

    // Built from this template's own distribution, independent of either
    // statement, so a source ARN pointed at a different distribution fails
    // both the ReadOnly and the ListBucket assertions below.
    const expectedSourceCondition = {
      StringEquals: {
        'AWS:SourceArn': {
          'Fn::Join': [
            '',
            [
              'arn:',
              { Ref: 'AWS::Partition' },
              ':cloudfront::',
              { Ref: 'AWS::AccountId' },
              ':distribution/',
              { Ref: distributionLogicalId },
            ],
          ],
        },
        'AWS:SourceAccount': { Ref: 'AWS::AccountId' },
      },
    };

    template.hasResourceProperties('AWS::S3::BucketPolicy', {
      PolicyDocument: {
        Statement: Match.arrayWith([
          Match.objectLike({
            Sid: 'AllowCloudFrontServicePrincipalReadOnly',
            Effect: 'Allow',
            Action: 's3:GetObject',
            Condition: expectedSourceCondition,
          }),
          Match.objectLike({
            Sid: 'AllowCloudFrontServicePrincipalListBucket',
            Effect: 'Allow',
            Action: 's3:ListBucket',
            Principal: {
              Service: 'cloudfront.amazonaws.com',
            },
            Resource: {
              'Fn::GetAtt': [bucketLogicalId, 'Arn'],
            },
            Condition: expectedSourceCondition,
          }),
        ]),
      },
    });

    template.resourceCountIs('AWS::S3::BucketPolicy', 1);
  });
});

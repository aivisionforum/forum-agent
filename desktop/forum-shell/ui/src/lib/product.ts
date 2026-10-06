import product from '../../../../../profiles/ai-vision-forum/product.json';

export const productName = product.build_identity.product_name;
export const developmentVersion = product.development_version;

// Presentation assets belong to the build's brand, never to a meeting setting.
export { default as brandLogoUrl } from '../../../icons/logo-mark.png';

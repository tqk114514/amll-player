import { Spinner } from "@radix-ui/themes";
import {
	createBrowserRouter,
	createRoutesFromElements,
	Route,
} from "react-router-dom";
import ErrorPage from "./pages/error/index.tsx";

/** 懒加载路由模块尚未就绪时的水合占位,避免 react-router 输出 HydrateFallback 告警 */
const RouteFallback = () => (
	<div
		style={{
			width: "100%",
			height: "100vh",
			display: "grid",
			placeItems: "center",
		}}
	>
		<Spinner size="3" />
	</div>
);

export const router = createBrowserRouter(
	createRoutesFromElements(
		<>
			<Route
				path="/"
				lazy={() => import("./pages/main/index.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/settings"
				lazy={() => import("./pages/settings/index.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/search"
				lazy={() => import("./pages/search/index.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/playlist/:id"
				lazy={() => import("./pages/playlist")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/song/:id"
				lazy={() => import("./pages/song/index.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/amll-dev/mg-edit"
				lazy={() => import("./pages/amll-dev/mg-edit.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/amll-dev"
				lazy={() => import("./pages/amll-dev/index.tsx")}
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			/>
			<Route
				path="/ws"
				errorElement={<ErrorPage />}
				hydrateFallbackElement={<RouteFallback />}
			>
				<Route
					path="recv"
					lazy={() => import("./pages/ws/recv.tsx")}
					errorElement={<ErrorPage />}
					hydrateFallbackElement={<RouteFallback />}
				/>
				<Route
					path="send"
					lazy={() => import("./pages/ws/send.tsx")}
					errorElement={<ErrorPage />}
					hydrateFallbackElement={<RouteFallback />}
				/>
			</Route>
		</>,
	),
);
